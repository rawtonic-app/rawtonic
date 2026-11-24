//! Progress output must not prevent a command from finishing when a consumer closes its pipe.
// Adapted from CyberTimon/RapidRAW b64adfc6, AGPL-3.0.
#[macro_export]
macro_rules! cli_println {
    ($($arg:tt)*) => {{ use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*); }};
}
#[macro_export]
macro_rules! cli_eprintln {
    ($($arg:tt)*) => {{ use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*); }};
}

#[cfg(test)]
mod tests {
    #[test]
    fn closed_pipe_child() {
        if std::env::var_os("RAWTONIC_CLOSED_PIPE_TEST").is_none() {
            return;
        }
        use std::io::Write as _;
        writeln!(std::io::stdout(), "READY").unwrap();
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        // Outlast the pipe buffer even if the parent has not closed it yet.
        for _ in 0..20000 {
            cli_println!("progress");
            cli_eprintln!("diagnostic");
        }
        std::process::exit(0);
    }

    #[test]
    fn closed_output_pipes_exit_cleanly() {
        use std::io::{BufRead, Write};
        use std::process::{Command, Stdio};
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["closed_pipe_child", "--nocapture"])
            .env("RAWTONIC_CLOSED_PIPE_TEST", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            assert!(
                output.read_line(&mut line).unwrap() > 0,
                "child ended before ready"
            );
            if line.trim() == "READY" {
                break;
            }
            line.clear();
        }
        drop(output);
        drop(child.stderr.take());
        child.stdin.take().unwrap().write_all(b"go\n").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "closed pipe child: {status}");
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("closed pipe child hung");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
