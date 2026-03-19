use std::io::{self, Write};
use std::process::{Child, Stdio};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// Cross-platform terminal session for SSH proxying.
///
/// Handles raw mode, PTY/pipe setup, and terminal resize propagation.
/// On Unix: allocates a PTY pair so SSH sees a real terminal.
/// On Windows: uses piped stdin with console raw mode.
pub struct Terminal {
    #[cfg(unix)]
    inner: unix::UnixTerminal,
    #[cfg(windows)]
    inner: windows::WindowsTerminal,
}

impl Terminal {
    /// Enter raw mode and prepare SSH stdin source.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            inner: unix::UnixTerminal::new()?,
            #[cfg(windows)]
            inner: windows::WindowsTerminal::new()?,
        })
    }

    /// Get Stdio for SSH command's stdin.
    pub fn ssh_stdin(&mut self) -> Stdio {
        self.inner.ssh_stdin()
    }

    /// After SSH spawns: return a writer for sending intercepted input,
    /// and set up terminal resize handling.
    pub fn finalize(&mut self, child: &mut Child) -> Box<dyn Write + Send> {
        self.inner.finalize(child)
    }

    /// Restore terminal settings. Must be called before process exit
    /// (std::process::exit does not run Drop).
    pub fn cleanup(&mut self) {
        self.inner.cleanup();
    }
}
