//! `StdQmp`: [`Qmp`] over a tokio unix socket. Each step (connect, greeting, each command's reply)
//! gets [`STEP_TIMEOUT`]; lines are read with a size bound; events are skipped.

use std::future::Future;
use std::path::Path;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;

use crate::layout::SocketPath;
use crate::plan::{QmpCommand, QmpGreeting, QmpReply};
use crate::ports::Cause;
use crate::ports::qmp::{MAX_LINE, Qmp, QmpError, QmpPhase, STEP_TIMEOUT};

/// The real control socket.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct StdQmp;

impl Qmp for StdQmp {
    fn powerdown(&self, control: &SocketPath) -> impl Future<Output = Result<(), QmpError>> + Send {
        let control = control.clone();
        async move { powerdown(&control).await }
    }
}

async fn powerdown(control: &SocketPath) -> Result<(), QmpError> {
    let stream = step(QmpPhase::Connect, async {
        UnixStream::connect(Path::new(control.get().get()))
            .await
            .map_err(|error| QmpError::Connect {
                socket: control.clone(),
                cause: Cause::of(&error),
            })
    })
    .await?;
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let phase = QmpPhase::Greeting;
    let line = step(phase, read_line(&mut read, phase)).await?;
    QmpGreeting::parse(&line).map_err(QmpError::Greeting)?;
    for command in [QmpCommand::Capabilities, QmpCommand::Powerdown] {
        step(
            QmpPhase::Command(command),
            converse(&mut read, &mut write, command),
        )
        .await?;
    }
    Ok(())
}

/// Sends `command` and reads until its own reply arrives, skipping events.
async fn converse(
    read: &mut BufReader<OwnedReadHalf>,
    write: &mut OwnedWriteHalf,
    command: QmpCommand,
) -> Result<(), QmpError> {
    let phase = QmpPhase::Command(command);
    write
        .write_all(command.line().as_bytes())
        .await
        .map_err(|error| QmpError::Io {
            phase,
            cause: Cause::of(&error),
        })?;
    loop {
        let line = read_line(read, phase).await?;
        match QmpReply::parse(&line).map_err(|error| QmpError::Reply { command, error })? {
            QmpReply::Event => {}
            QmpReply::Return => return Ok(()),
            QmpReply::Error { class, desc } => {
                return Err(QmpError::Refused {
                    command,
                    class,
                    desc,
                });
            }
        }
    }
}

/// `work`, or `Timeout` if it takes longer than [`STEP_TIMEOUT`].
async fn step<T>(
    phase: QmpPhase,
    work: impl Future<Output = Result<T, QmpError>>,
) -> Result<T, QmpError> {
    timeout(STEP_TIMEOUT, work)
        .await
        .unwrap_or(Err(QmpError::Timeout { phase }))
}

/// The next line without its newline, at most [`MAX_LINE`] bytes long.
async fn read_line(
    read: &mut BufReader<OwnedReadHalf>,
    phase: QmpPhase,
) -> Result<Vec<u8>, QmpError> {
    let mut line = Vec::new();
    loop {
        let available = read.fill_buf().await.map_err(|error| QmpError::Io {
            phase,
            cause: Cause::of(&error),
        })?;
        if available.is_empty() {
            return Err(QmpError::Closed { phase });
        }
        let (taken, found) = match available.iter().position(|&byte| byte == b'\n') {
            Some(at) => (at.saturating_add(1), true),
            None => (available.len(), false),
        };
        line.extend(available.iter().take(taken));
        read.consume(taken);
        if line.len() > MAX_LINE.saturating_add(usize::from(found)) {
            return Err(QmpError::TooLong { phase });
        }
        if found {
            line.pop();
            return Ok(line);
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::future::pending;
    use std::io;
    use std::time::Duration;

    use tokio::net::UnixListener;

    use super::*;
    use crate::diagnostic::Diagnostic as _;
    use crate::domain::AbsPath;
    use crate::plan::{GreetingError, QmpDesc, QmpErrorClass, ReplyError};
    use crate::testing::TempDir;

    const GREETING: &str = r#"{"QMP": {"version": {"qemu": {"micro": 0, "minor": 2, "major": 9}, "package": ""}, "capabilities": ["oob"]}}"#;
    const RETURN: &str = r#"{"return": {}}"#;
    const EVENT: &str =
        r#"{"event": "POWERDOWN", "data": {}, "timestamp": {"seconds": 1, "microseconds": 2}}"#;

    type Reader = BufReader<OwnedReadHalf>;

    fn socket_in(temp: &TempDir) -> SocketPath {
        let path = format!("{}/q.sock", temp.path().to_str().unwrap());
        SocketPath::parse(AbsPath::parse(path).unwrap()).unwrap()
    }

    fn runtime(paused: bool) -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(paused)
            .build()
            .unwrap()
    }

    /// Runs the powerdown conversation against `server`, which gets the accepted connection; the
    /// result is the client's and the lines the server read, if it returned them.
    fn converse_with<F, Fut>(paused: bool, server: F) -> Result<(), QmpError>
    where
        F: FnOnce(Reader, OwnedWriteHalf) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let temp = TempDir::new("qmp").unwrap();
        let socket = socket_in(&temp);
        let runtime = runtime(paused);
        let _entered = runtime.enter();
        let listener = UnixListener::bind(socket.get().get()).unwrap();
        runtime.block_on(async {
            let task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, write) = stream.into_split();
                server(BufReader::new(read), write).await;
            });
            let result = StdQmp.powerdown(&socket).await;
            task.abort();
            result
        })
    }

    async fn send(write: &mut OwnedWriteHalf, line: &str) {
        write.write_all(line.as_bytes()).await.unwrap();
        write.write_all(b"\n").await.unwrap();
    }

    async fn receive(read: &mut Reader) -> String {
        let mut line = String::new();
        read.read_line(&mut line).await.unwrap();
        line
    }

    #[test]
    fn a_normal_conversation_negotiates_then_powers_down_skipping_events() {
        let temp = TempDir::new("qmp-happy").unwrap();
        let socket = socket_in(&temp);
        let runtime = runtime(false);
        let _entered = runtime.enter();
        let listener = UnixListener::bind(socket.get().get()).unwrap();
        let (result, received) = runtime.block_on(async {
            let task = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                send(&mut write, GREETING).await;
                let first = receive(&mut read).await;
                send(&mut write, EVENT).await;
                send(&mut write, RETURN).await;
                let second = receive(&mut read).await;
                send(&mut write, EVENT).await;
                send(&mut write, EVENT).await;
                send(&mut write, RETURN).await;
                [first, second]
            });
            (StdQmp.powerdown(&socket).await, task.await.unwrap())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(
            received,
            [
                "{\"execute\":\"qmp_capabilities\"}\n".to_owned(),
                "{\"execute\":\"system_powerdown\"}\n".to_owned()
            ]
        );
    }

    #[test]
    fn a_reply_delivered_in_pieces_is_put_together() {
        let result = converse_with(false, |mut read, mut write| async move {
            let (head, tail) = GREETING.as_bytes().split_at(10);
            write.write_all(head).await.unwrap();
            write.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
            write.write_all(tail).await.unwrap();
            write.write_all(b"\n").await.unwrap();
            for _ in 0..2 {
                receive(&mut read).await;
                write.write_all(b"{\"ret").await.unwrap();
                write.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
                write.write_all(b"urn\": {}}\n").await.unwrap();
            }
        });
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn a_missing_socket_is_a_connect_error() {
        let temp = TempDir::new("qmp-missing").unwrap();
        let socket = socket_in(&temp);
        let result = runtime(false).block_on(StdQmp.powerdown(&socket));
        assert_eq!(
            result,
            Err(QmpError::Connect {
                socket: socket.clone(),
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(2)
                }
            })
        );
    }

    #[test]
    fn a_greeting_that_is_not_qmp_is_refused() {
        let cases = [
            ("not json at all", GreetingError::NotJson),
            (r#"{"hello": 1}"#, GreetingError::NotQmp),
            ("[]", GreetingError::NotQmp),
        ];
        for (greeting, expected) in cases {
            let result = converse_with(false, move |_read, mut write| async move {
                send(&mut write, greeting).await;
                pending::<()>().await;
            });
            assert_eq!(result, Err(QmpError::Greeting(expected)), "{greeting}");
        }
    }

    #[test]
    fn a_connection_closed_at_any_point_is_reported_with_the_step() {
        // The server is gone when the command is written: the write fails with EPIPE, or, when it
        // lands before the close is seen, the read that follows hits the end of the stream.
        let after_greeting = converse_with(false, |_read, mut write| async move {
            send(&mut write, GREETING).await;
        });
        let phase = QmpPhase::Command(QmpCommand::Capabilities);
        assert!(
            after_greeting
                == Err(QmpError::Io {
                    phase,
                    cause: Cause {
                        kind: io::ErrorKind::BrokenPipe,
                        errno: Some(32)
                    }
                })
                || after_greeting == Err(QmpError::Closed { phase }),
            "{after_greeting:?}"
        );
        let before_greeting = converse_with(false, |_read, _write| async move {});
        assert_eq!(
            before_greeting,
            Err(QmpError::Closed {
                phase: QmpPhase::Greeting
            })
        );
        let after_command = converse_with(false, |mut read, mut write| async move {
            send(&mut write, GREETING).await;
            receive(&mut read).await;
        });
        assert_eq!(
            after_command,
            Err(QmpError::Closed {
                phase: QmpPhase::Command(QmpCommand::Capabilities)
            })
        );
        let after_capabilities = converse_with(false, |mut read, mut write| async move {
            send(&mut write, GREETING).await;
            receive(&mut read).await;
            send(&mut write, RETURN).await;
            receive(&mut read).await;
        });
        assert_eq!(
            after_capabilities,
            Err(QmpError::Closed {
                phase: QmpPhase::Command(QmpCommand::Powerdown)
            })
        );
    }

    #[test]
    fn an_error_reply_is_a_refusal_naming_the_command_the_class_and_a_clean_description() {
        let capabilities = converse_with(false, |mut read, mut write| async move {
            send(&mut write, GREETING).await;
            receive(&mut read).await;
            send(
                &mut write,
                r#"{"error": {"class": "CommandNotFound", "desc": "no such command"}}"#,
            )
            .await;
        });
        assert_eq!(
            capabilities,
            Err(QmpError::Refused {
                command: QmpCommand::Capabilities,
                class: QmpErrorClass::CommandNotFound,
                desc: QmpDesc::of("no such command"),
            })
        );
        let powerdown = converse_with(false, |mut read, mut write| async move {
            send(&mut write, GREETING).await;
            receive(&mut read).await;
            send(&mut write, RETURN).await;
            receive(&mut read).await;
            send(
                &mut write,
                r#"{"error": {"class": "GenericError", "desc": "\u001b[31mred\nnew"}}"#,
            )
            .await;
        });
        let clean = QmpDesc::of("\u{1b}[31mred\nnew");
        assert_eq!(clean.as_str(), "[31mrednew");
        assert_eq!(
            powerdown,
            Err(QmpError::Refused {
                command: QmpCommand::Powerdown,
                class: QmpErrorClass::GenericError,
                desc: clean,
            })
        );
        assert_eq!(
            powerdown.unwrap_err().message(),
            "qemu refused system_powerdown: GenericError: [31mrednew"
        );
    }

    #[test]
    fn a_reply_of_an_unexpected_shape_is_a_typed_error() {
        let cases = [
            ("garbage", ReplyError::NotJson),
            (r#"{"unexpected": 1}"#, ReplyError::NotMessage),
            (
                r#"{"error": {"class": "Odd", "desc": "x"}}"#,
                ReplyError::UnknownClass {
                    class: "Odd".to_owned(),
                },
            ),
            (r#"{"error": "x"}"#, ReplyError::BadError),
        ];
        for (reply, expected) in cases {
            let result = converse_with(false, move |mut read, mut write| async move {
                send(&mut write, GREETING).await;
                receive(&mut read).await;
                send(&mut write, reply).await;
                pending::<()>().await;
            });
            assert_eq!(
                result,
                Err(QmpError::Reply {
                    command: QmpCommand::Capabilities,
                    error: expected
                }),
                "{reply}"
            );
        }
    }

    #[test]
    fn a_silent_qemu_times_out_at_the_step_it_was_silent_in() {
        let greeting = converse_with(true, |read, write| async move {
            let _kept_open = (read, write);
            pending::<()>().await;
        });
        assert_eq!(
            greeting,
            Err(QmpError::Timeout {
                phase: QmpPhase::Greeting
            })
        );
        let capabilities = converse_with(true, |_read, mut write| async move {
            send(&mut write, GREETING).await;
            pending::<()>().await;
        });
        assert_eq!(
            capabilities,
            Err(QmpError::Timeout {
                phase: QmpPhase::Command(QmpCommand::Capabilities)
            })
        );
        let powerdown = converse_with(true, |mut read, mut write| async move {
            send(&mut write, GREETING).await;
            receive(&mut read).await;
            send(&mut write, RETURN).await;
            // Events keep coming but the answer never does.
            for _ in 0..3 {
                send(&mut write, EVENT).await;
            }
            pending::<()>().await;
        });
        assert_eq!(
            powerdown,
            Err(QmpError::Timeout {
                phase: QmpPhase::Command(QmpCommand::Powerdown)
            })
        );
    }

    #[test]
    fn a_step_that_never_finishes_times_out_after_the_step_timeout() {
        let result: Result<(), QmpError> =
            runtime(true).block_on(step(QmpPhase::Connect, pending()));
        assert_eq!(
            result,
            Err(QmpError::Timeout {
                phase: QmpPhase::Connect
            })
        );
    }

    #[test]
    fn lines_are_bounded_and_the_bound_is_exact() {
        let phase = QmpPhase::Greeting;
        let read_with = |bytes: Vec<u8>| {
            runtime(false).block_on(async move {
                let (client, server) = UnixStream::pair().unwrap();
                let (read, _keep) = client.into_split();
                let (_, mut write) = server.into_split();
                let writer = tokio::spawn(async move {
                    let _ = write.write_all(&bytes).await;
                    // Closing ends the stream for the reader.
                });
                let mut read = BufReader::new(read);
                let result = read_line(&mut read, phase).await;
                writer.abort();
                result
            })
        };
        let exactly = [vec![b'a'; MAX_LINE], vec![b'\n']].concat();
        assert_eq!(read_with(exactly).map(|line| line.len()), Ok(MAX_LINE));
        let over = [vec![b'a'; MAX_LINE + 1], vec![b'\n']].concat();
        assert_eq!(read_with(over), Err(QmpError::TooLong { phase }));
        let endless = vec![b'a'; MAX_LINE + 10];
        assert_eq!(read_with(endless), Err(QmpError::TooLong { phase }));
        assert_eq!(
            read_with(b"first\nsecond\n".to_vec()),
            Ok(b"first".to_vec())
        );
        assert_eq!(
            read_with(b"no newline".to_vec()),
            Err(QmpError::Closed { phase })
        );
    }
}
