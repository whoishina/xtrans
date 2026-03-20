use std::io::{self, Write};
use std::process::{Child, Command, Stdio};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

const INVALID_HANDLE: HANDLE = -1isize as HANDLE;

pub struct WindowsTerminal {
    stdin_handle: HANDLE,
    original_in_mode: u32,
    stdout_handle: HANDLE,
    original_out_mode: Option<u32>,
    /// False when running in non-console terminals (Git Bash/mintty, MSYS2).
    /// In that case, stdin is a pipe from the terminal emulator and already
    /// provides raw bytes — no Console API manipulation needed.
    is_console: bool,
}

impl WindowsTerminal {
    pub fn new() -> io::Result<Self> {
        unsafe {
            let stdin_handle = GetStdHandle(STD_INPUT_HANDLE);
            let stdout_handle = GetStdHandle(STD_OUTPUT_HANDLE);

            // Detect whether stdin is a real Windows console handle.
            // Git Bash (mintty) and MSYS2 use PTY pipes — GetConsoleMode
            // fails on those handles.
            let mut original_in_mode: u32 = 0;
            let is_console = stdin_handle != INVALID_HANDLE
                && GetConsoleMode(stdin_handle, &mut original_in_mode) != 0;

            let mut original_out_mode: Option<u32> = None;

            if is_console {
                // Real Windows console (cmd.exe, PowerShell, Windows Terminal):
                // disable line input, echo, processed input.
                // Enable VT input so byte 0x16 (Ctrl+V) passes through
                // instead of being consumed by the console as paste.
                let _ = SetConsoleMode(stdin_handle, ENABLE_VIRTUAL_TERMINAL_INPUT);

                // Enable VT processing on stdout for proper ANSI output
                if stdout_handle != INVALID_HANDLE {
                    let mut mode: u32 = 0;
                    if GetConsoleMode(stdout_handle, &mut mode) != 0 {
                        let _ = SetConsoleMode(
                            stdout_handle,
                            mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                        );
                        original_out_mode = Some(mode);
                    }
                }
            }
            // else: non-console terminal (Git Bash/mintty, MSYS2, Cygwin).
            // stdin is a pipe from the terminal emulator which already provides
            // raw bytes with VT sequences. No console manipulation needed.

            Ok(Self {
                stdin_handle,
                original_in_mode,
                stdout_handle,
                original_out_mode,
                is_console,
            })
        }
    }

    pub fn ssh_stdin(&mut self) -> Stdio {
        // On Windows, SSH reads terminal size from its console (stdout),
        // so piped stdin does not break terminal size detection.
        Stdio::piped()
    }

    pub fn finalize(&mut self, child: &mut Child) -> Box<dyn Write + Send> {
        let stdin = child.stdin.take().expect("SSH stdin pipe");
        Box::new(stdin)
    }

    pub fn configure_ssh_command(&self, _cmd: &mut Command) {
        // Nothing needed on Windows — SSH uses console handles directly.
    }

    pub fn cleanup(&mut self) {
        if !self.is_console {
            return;
        }
        unsafe {
            SetConsoleMode(self.stdin_handle, self.original_in_mode);
            if let Some(mode) = self.original_out_mode {
                SetConsoleMode(self.stdout_handle, mode);
            }
        }
    }
}
