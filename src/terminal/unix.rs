use std::io::{self, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

// PTY master fd and SSH child PID — accessed by async-signal-safe SIGWINCH handler
static PTY_MASTER_FD: AtomicI32 = AtomicI32::new(-1);
static SSH_CHILD_PID: AtomicI32 = AtomicI32::new(-1);

unsafe extern "C" {
    fn openpty(
        amaster: *mut libc::c_int,
        aslave: *mut libc::c_int,
        name: *mut libc::c_char,
        termp: *const libc::termios,
        winp: *const libc::winsize,
    ) -> libc::c_int;
}

pub struct UnixTerminal {
    master_fd: libc::c_int,
    slave_fd: Option<libc::c_int>,
    original_termios: libc::termios,
}

impl UnixTerminal {
    pub fn new() -> io::Result<Self> {
        let ws = get_terminal_size();
        let (master_fd, slave_fd) = create_pty(&ws)?;

        // Redundant but guarantees correctness on all POSIX systems
        set_pty_size(slave_fd, &ws);

        // Prevent master fd from leaking into SSH child process
        set_cloexec(master_fd);

        // Do NOT set raw mode on PTY slave — SSH reads its termios and
        // forwards them to the remote PTY. Default settings (OPOST+ONLCR on)
        // ensure the remote properly translates \n → \r\n.
        // SSH will put the slave into raw mode itself after reading defaults.

        // Install SIGWINCH handler before entering raw mode
        PTY_MASTER_FD.store(master_fd, Ordering::Relaxed);
        install_sigwinch_handler();

        let original_termios = enter_raw_mode();

        Ok(Self {
            master_fd,
            slave_fd: Some(slave_fd),
            original_termios,
        })
    }

    pub fn ssh_stdin(&mut self) -> Stdio {
        let fd = self.slave_fd.take().expect("ssh_stdin called twice");
        unsafe { Stdio::from(std::os::fd::OwnedFd::from_raw_fd(fd)) }
    }

    pub fn finalize(&mut self, child: &mut Child) -> Box<dyn Write + Send> {
        SSH_CHILD_PID.store(child.id() as i32, Ordering::Relaxed);

        // Dup master fd: one for writing (input_loop), one for reading (output pump).
        // PTY master is bidirectional — read returns slave's output (SSH's /dev/tty
        // writes), write sends to slave's input (SSH reads from stdin//dev/tty).
        let master_read_fd = unsafe { libc::dup(self.master_fd) };

        // Spawn output pump: reads SSH's /dev/tty output from PTY master and
        // forwards to the real terminal. Without this, host key prompts and
        // password prompts are invisible (stuck in PTY master buffer).
        if master_read_fd >= 0 {
            set_cloexec(master_read_fd);
            std::thread::spawn(move || {
                let mut master_read =
                    unsafe { std::fs::File::from_raw_fd(master_read_fd) };
                let stdout = io::stdout();
                let mut out = stdout.lock();
                let mut buf = [0u8; 4096];
                loop {
                    match Read::read(&mut master_read, &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let _ = out.write_all(&buf[..n]);
                            let _ = out.flush();
                        }
                    }
                }
            });
        }

        // Transfer ownership of master fd to File for input writing
        let file = unsafe { std::fs::File::from_raw_fd(self.master_fd) };
        self.master_fd = -1;

        Box::new(file)
    }

    pub fn configure_ssh_command(&self, cmd: &mut Command) {
        // Make the PTY slave the controlling terminal for SSH.
        // Without this, SSH opens /dev/tty (the real terminal) for interactive
        // prompts (host key verification, password), but our input_loop is
        // reading from the same real terminal — SSH never receives the input.
        // After setsid + TIOCSCTTY, SSH's /dev/tty points to the PTY slave,
        // so input from our PTY master reaches SSH's prompt reads.
        unsafe {
            cmd.pre_exec(|| {
                // New session — detach from parent's controlling terminal
                libc::setsid();
                // Make stdin (PTY slave, fd 0) the controlling terminal
                libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0);
                Ok(())
            });
        }
    }

    pub fn cleanup(&mut self) {
        restore_termios(&self.original_termios);
        PTY_MASTER_FD.store(-1, Ordering::Relaxed);
        SSH_CHILD_PID.store(-1, Ordering::Relaxed);

        if self.master_fd >= 0 {
            unsafe {
                libc::close(self.master_fd);
            }
            self.master_fd = -1;
        }
    }
}

// ---------------------------------------------------------------------------
// PTY
// ---------------------------------------------------------------------------

fn create_pty(ws: &libc::winsize) -> io::Result<(libc::c_int, libc::c_int)> {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    let ret = unsafe {
        openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            ws as *const libc::winsize,
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((master, slave))
}

fn get_terminal_size() -> libc::winsize {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        libc::ioctl(
            libc::STDIN_FILENO,
            libc::TIOCGWINSZ,
            &mut ws as *mut libc::winsize,
        );
        ws
    }
}

fn set_pty_size(fd: libc::c_int, ws: &libc::winsize) {
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, ws as *const libc::winsize);
    }
}

fn set_cloexec(fd: libc::c_int) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
    }
}

// ---------------------------------------------------------------------------
// SIGWINCH — propagate terminal resize: real terminal → PTY → SSH → remote
// ---------------------------------------------------------------------------

/// TIOCSWINSZ on the PTY master delivers SIGWINCH to the slave's process
/// group (SSH). We also explicitly signal SSH in case it is not in the
/// foreground process group of the slave.
extern "C" fn sigwinch_handler(_sig: libc::c_int) {
    let fd = PTY_MASTER_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            libc::ioctl(
                libc::STDIN_FILENO,
                libc::TIOCGWINSZ,
                &mut ws as *mut libc::winsize,
            );
            libc::ioctl(fd, libc::TIOCSWINSZ, &ws as *const libc::winsize);
        }
    }
    let pid = SSH_CHILD_PID.load(Ordering::Relaxed);
    if pid > 0 {
        unsafe {
            libc::kill(pid, libc::SIGWINCH);
        }
    }
}

fn install_sigwinch_handler() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = sigwinch_handler as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGWINCH, &sa, std::ptr::null_mut());
    }
}

// ---------------------------------------------------------------------------
// Terminal raw mode (POSIX termios)
// ---------------------------------------------------------------------------

fn enter_raw_mode() -> libc::termios {
    unsafe {
        let mut original: libc::termios = std::mem::zeroed();
        libc::tcgetattr(libc::STDIN_FILENO, &mut original);

        let mut raw = original;
        libc::cfmakeraw(&mut raw);

        // Re-enable output processing so \n → \r\n translation works.
        // cfmakeraw clears OPOST, but SSH writes local messages (host key
        // prompts, errors) directly to stderr with bare \n. Without OPOST
        // the cursor moves down without returning to column 0.
        // Remote output already contains \r\n (from remote PTY's OPOST),
        // so the extra \r from ONLCR is harmless (\r\r\n = \r\n visually).
        raw.c_oflag |= libc::OPOST | libc::ONLCR;

        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &raw);

        original
    }
}

fn restore_termios(original: &libc::termios) {
    unsafe {
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, original);
    }
}
