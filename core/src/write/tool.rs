//! One long-lived ExifTool, kept open between photos. A cold start costs around 200 ms, a write
//! through a process that is already up around 7 ms, and a bulk pass is thousands of photos.
//!
//! The protocol is ExifTool's own: arguments one per line into its standard input, `-execute<n>`
//! to run them, and `{ready<n>}` on standard output when it is done. `-echo4` puts a matching
//! marker on standard error, so both streams can be drained to a known end rather than guessed at.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use super::{Error, Result};

/// Long enough for ExifTool to rewrite the largest photo in the library, short enough that a
/// wedged process does not hold up a bulk pass for ever.
const TIMEOUT: Duration = Duration::from_secs(60);

/// What one command said.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub out: String,
    pub err: String,
}

impl Reply {
    /// ExifTool counts what it changed on standard output; anything else is a failure.
    pub fn updated(&self) -> bool {
        self.out.lines().any(|line| {
            let line = line.trim();
            line.ends_with("image files updated") && !line.starts_with('0')
        })
    }

    /// The line worth showing a person: the error that stopped it, before any warning that came
    /// first.
    pub fn complaint(&self) -> String {
        let lines = || self.err.lines().chain(self.out.lines()).map(str::trim);
        lines()
            .find(|line| line.starts_with("Error:"))
            .or_else(|| lines().find(|line| line.starts_with("Warning:") || *line == "Nothing to do."))
            .unwrap_or("exiftool changed nothing and gave no reason")
            .trim_start_matches("Error:")
            .trim()
            .to_string()
    }
}

#[derive(Debug)]
pub struct Tool {
    running: Option<Running>,
    commands: u64,
}

#[derive(Debug)]
struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    out: Receiver<String>,
    err: Receiver<String>,
}

impl Default for Tool {
    fn default() -> Tool {
        Tool::new()
    }
}

impl Tool {
    pub fn new() -> Tool {
        Tool {
            running: None,
            commands: 0,
        }
    }

    /// How many commands this driver has sent, over however many processes.
    pub fn commands(&self) -> u64 {
        self.commands
    }

    /// One command. `args` is what ExifTool gets, one argument per element, the file last.
    ///
    /// A process that has died is started again and the command repeated once: every command the
    /// engine sends is a read, or a write of a temporary copy, so repeating one is safe.
    pub fn run(&mut self, args: &[String]) -> Result<Reply> {
        match self.attempt(args) {
            Err(Error::ToolGone(why)) => {
                tracing::warn!(why, "exiftool went away, starting it again");
                self.stop();
                self.attempt(args)
            }
            other => other,
        }
    }

    fn attempt(&mut self, args: &[String]) -> Result<Reply> {
        if let Some(bad) = args.iter().find(|arg| arg.contains('\n')) {
            return Err(Error::Refusing(format!("a newline in an exiftool argument: {bad:?}")));
        }
        if self.running.is_none() {
            self.running = Some(Running::start()?);
        }
        self.commands += 1;
        let mark = self.commands;
        let running = self.running.as_mut().expect("a process we just started");

        let mut request = String::new();
        for arg in args {
            request.push_str(arg);
            request.push('\n');
        }
        request.push_str("-echo4\n");
        request.push_str(&format!("{{done{mark}}}\n-execute{mark}\n"));

        let stdin = running
            .stdin
            .as_mut()
            .ok_or_else(|| Error::ToolGone("its input is closed".to_string()))?;
        stdin
            .write_all(request.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|error| Error::ToolGone(error.to_string()))?;

        let out = drain(&running.out, &format!("{{ready{mark}}}"))?;
        let err = drain(&running.err, &format!("{{done{mark}}}"))?;
        Ok(Reply { out, err })
    }

    /// Ends the process, so the next command starts a fresh one.
    pub fn stop(&mut self) {
        let Some(mut running) = self.running.take() else {
            return;
        };
        if let Some(mut stdin) = running.stdin.take() {
            let _ = stdin.write_all(b"-stay_open\nFalse\n");
            let _ = stdin.flush();
        }
        for _ in 0..50 {
            match running.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = running.child.kill();
        let _ = running.child.wait();
    }
}

impl Drop for Tool {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Running {
    fn start() -> Result<Running> {
        let mut child = Command::new("exiftool")
            .args(["-stay_open", "True", "-@", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| Error::NoTool(error.to_string()))?;
        let missing = || Error::NoTool("exiftool gave us no pipes".to_string());
        let stdin = child.stdin.take().ok_or_else(missing)?;
        let out = lines(child.stdout.take().ok_or_else(missing)?);
        let err = lines(child.stderr.take().ok_or_else(missing)?);
        Ok(Running {
            child,
            stdin: Some(stdin),
            out,
            err,
        })
    }
}

/// A blocking read cannot be given a deadline, so each stream is read by its own thread and the
/// lines come back through a channel the caller can wait on with one.
fn lines<R: Read + Send + 'static>(source: R) -> Receiver<String> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(source).lines().map_while(std::result::Result::ok) {
            if sender.send(line).is_err() {
                return;
            }
        }
    });
    receiver
}

/// Everything up to the marker. A closed stream means the process is gone; silence for the whole
/// timeout means it is wedged, and either way the caller gets a reason rather than a hang.
fn drain(stream: &Receiver<String>, marker: &str) -> Result<String> {
    let mut collected = String::new();
    loop {
        match stream.recv_timeout(TIMEOUT) {
            Ok(line) if line.trim() == marker => return Ok(collected),
            Ok(line) => {
                collected.push_str(&line);
                collected.push('\n');
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Error::ToolGone(format!("it stopped before {marker}")));
            }
            Err(RecvTimeoutError::Timeout) => return Err(Error::Stuck(TIMEOUT.as_secs())),
        }
    }
}

/// ExifTool is asked for a file by path, and a path that is not valid UTF-8 cannot go down a pipe.
pub fn as_argument(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Refusing(format!("{} is not a name we can pass on", path.display())))
}

#[cfg(test)]
mod reply_tests {
    use super::*;

    #[test]
    fn the_error_is_told_before_a_warning_that_came_first() {
        let reply = Reply {
            out: "    0 image files updated\n    1 files weren't updated due to errors".to_string(),
            err: "Warning: [minor] Fixed incorrect URI for xmlns:MicrosoftPhoto - a.jpg\n\
                  Error: [minor] MakerNotes offsets may be incorrect (fix or ignore?) - a.jpg"
                .to_string(),
        };
        assert!(!reply.updated());
        assert_eq!(
            reply.complaint(),
            "[minor] MakerNotes offsets may be incorrect (fix or ignore?) - a.jpg"
        );
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;

    fn photo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("photomanager-tool-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, include_bytes!("../fixtures/p01.jpg")).unwrap();
        file
    }

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn runs_a_command_and_sees_the_end_of_both_streams() {
        let file = photo("both-streams");
        let mut tool = Tool::new();
        let reply = tool
            .run(&args(&[
                "-overwrite_original",
                "-XMP-xmp:Rating=3",
                file.to_str().unwrap(),
            ]))
            .unwrap();
        assert!(reply.updated(), "{reply:?}");
        assert!(reply.err.is_empty(), "{reply:?}");

        let read = tool
            .run(&args(&["-s3", "-XMP-xmp:Rating", file.to_str().unwrap()]))
            .unwrap();
        assert_eq!(read.out.trim(), "3");
    }

    #[test]
    fn a_failure_is_a_reason_not_an_error() {
        let mut tool = Tool::new();
        let reply = tool
            .run(&args(&[
                "-overwrite_original",
                "-XMP-xmp:Rating=3",
                "/nowhere/at/all.jpg",
            ]))
            .unwrap();
        assert!(!reply.updated());
        assert!(reply.complaint().contains("File not found"), "{reply:?}");
    }

    #[test]
    fn the_process_survives_hundreds_of_commands() {
        let file = photo("hundreds");
        let mut tool = Tool::new();
        for round in 0..300 {
            let reply = tool
                .run(&args(&[
                    "-overwrite_original",
                    &format!("-XMP-xmp:Rating={}", round % 6),
                    file.to_str().unwrap(),
                ]))
                .unwrap();
            assert!(reply.updated(), "round {round}: {reply:?}");
        }
        assert_eq!(tool.commands(), 300);
    }

    #[test]
    fn comes_back_after_being_killed() {
        let file = photo("killed");
        let mut tool = Tool::new();
        tool.run(&args(&["-s3", "-Model", file.to_str().unwrap()])).unwrap();

        let pid = tool.running.as_ref().unwrap().child.id();
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap();

        let reply = tool
            .run(&args(&[
                "-overwrite_original",
                "-XMP-xmp:Rating=2",
                file.to_str().unwrap(),
            ]))
            .unwrap();
        assert!(reply.updated(), "{reply:?}");
        assert_ne!(tool.running.as_ref().unwrap().child.id(), pid, "a fresh process");
    }

    #[test]
    fn refuses_an_argument_it_cannot_send() {
        let mut tool = Tool::new();
        let error = tool
            .run(&args(&["-Comment=one\ntwo", "/tmp/whatever.jpg"]))
            .unwrap_err();
        assert!(matches!(error, Error::Refusing(_)), "{error:?}");
    }
}
