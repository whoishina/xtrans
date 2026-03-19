use std::io::{self, Write};
use std::process::{Child, Stdio};

use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

const INVALID_HANDLE: isize = -1;

pub struct WindowsTerminal {
    stdin_handle: isize,
    original_in_mode: u32,
    stdout_handle: isize,
    original_out_mode: Option<u32>,
}

impl WindowsTerminal {
    pub fn new() -> io::Result<Self> {
        unsafe {
            let stdin_handle = GetStdHandle(STD_INPUT_HANDLE);
            if stdin_handle == INVALID_HANDLE {
                return Err(io::Error::last_os_error());
            }

            let mut original_in_mode: u32 = 0;
            if GetConsoleMode(stdin_handle, &mut original_in_mode) == 0 {
                return Err(io::Error::last_os_error());
            }

            // Raw mode: disable line input, echo, processed input.
            // Enable VT input so special keys send escape sequences (byte 0x16
            // for Ctrl+V passes through instead of being consumed by the console).
            if SetConsoleMode(stdin_handle, ENABLE_VIRTUAL_TERMINAL_INPUT) == 0 {
                return Err(io::Error::last_os_error());
            }

            // Enable VT processing on stdout for proper ANSI output
            let stdout_handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let original_out_mode = if stdout_handle != INVALID_HANDLE {
                let mut mode: u32 = 0;
                if GetConsoleMode(stdout_handle, &mut mode) != 0 {
                    let _ = SetConsoleMode(
                        stdout_handle,
                        mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                    );
                    Some(mode)
                } else {
                    None
                }
            } else {
                None
            };

            Ok(Self {
                stdin_handle,
                original_in_mode,
                stdout_handle,
                original_out_mode,
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

    pub fn cleanup(&mut self) {
        unsafe {
            SetConsoleMode(self.stdin_handle, self.original_in_mode);
            if let Some(mode) = self.original_out_mode {
                SetConsoleMode(self.stdout_handle, mode);
            }
        }
    }
}
