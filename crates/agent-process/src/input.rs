use std::{io, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(super) async fn relay(
    source: &mut (impl AsyncRead + Unpin),
    output: &mut (impl AsyncWrite + Unpin),
    mut owner_closed: impl FnMut() -> io::Result<bool>,
) -> io::Result<()> {
    let mut buffer = [0; 8192];
    loop {
        let length = source.read(&mut buffer).await?;
        if length == 0 {
            return Ok(());
        }
        tokio::select! {
            biased;
            result = output.write_all(&buffer[..length]) => result?,
            result = wait_for_owner(&mut owner_closed) => return result,
        }
    }
}

async fn wait_for_owner(owner_closed: &mut impl FnMut() -> io::Result<bool>) -> io::Result<()> {
    while !owner_closed()? {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn owner_closed(pipe: &impl std::os::fd::AsRawFd) -> io::Result<bool> {
    // Darwin needs read readiness requested to report a pipe hangup, even
    // though unread input must remain untouched by this lifetime check.
    let mut descriptor = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut descriptor, 1, 0) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0)
}

#[cfg(windows)]
pub(super) use windows::owner_closed;

#[cfg(windows)]
mod windows {
    use std::{ffi::c_void, io, os::windows::io::AsRawHandle};

    const FILE_PIPE_LOCAL_INFORMATION: u32 = 24;
    const FILE_PIPE_CONNECTED_STATE: u32 = 3;

    #[repr(C)]
    #[derive(Default)]
    struct IoStatusBlock {
        status: isize,
        information: usize,
    }

    #[repr(C)]
    #[derive(Default)]
    struct PipeLocalInformation {
        pipe_type: u32,
        configuration: u32,
        maximum_instances: u32,
        current_instances: u32,
        inbound_quota: u32,
        read_data_available: u32,
        outbound_quota: u32,
        write_quota_available: u32,
        state: u32,
        end: u32,
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationFile(
            file: *mut c_void,
            status: *mut IoStatusBlock,
            information: *mut PipeLocalInformation,
            length: u32,
            class: u32,
        ) -> i32;
    }

    pub(crate) fn owner_closed(pipe: &impl AsRawHandle) -> io::Result<bool> {
        let mut status = IoStatusBlock::default();
        let mut information = PipeLocalInformation::default();
        let result = unsafe {
            NtQueryInformationFile(
                pipe.as_raw_handle(),
                &mut status,
                &mut information,
                std::mem::size_of::<PipeLocalInformation>() as u32,
                FILE_PIPE_LOCAL_INFORMATION,
            )
        };
        if result < 0 {
            return Err(io::Error::other(format!(
                "query lifetime pipe failed: {result:#x}"
            )));
        }
        Ok(information.state != FILE_PIPE_CONNECTED_STATE)
    }
}

#[cfg(not(any(unix, windows)))]
pub(super) fn owner_closed(_: &std::io::Stdin) -> io::Result<bool> {
    Err(io::Error::other(
        "process lifetime pipes are unsupported on this OS",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn owner_close_is_visible_with_unread_input() {
        let (mut reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"unread").unwrap();
        assert!(!owner_closed(&reader).unwrap());
        drop(writer);
        assert!(owner_closed(&reader).unwrap());
        let mut remaining = Vec::new();
        reader.read_to_end(&mut remaining).unwrap();
        assert_eq!(remaining, b"unread");
    }

    #[tokio::test]
    async fn owner_close_interrupts_a_blocked_write_without_draining_input() {
        let (mut reader, mut writer) = std::io::pipe().unwrap();
        writer.write_all(b"unread").unwrap();
        let source = [b'x'; 16384];
        let mut source = source.as_slice();
        let (mut output, _blocked_reader) = tokio::io::duplex(1);
        let mut copying = Box::pin(relay(&mut source, &mut output, || owner_closed(&reader)));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut copying)
                .await
                .is_err()
        );
        drop(writer);
        tokio::time::timeout(Duration::from_secs(1), copying)
            .await
            .unwrap()
            .unwrap();
        let mut remaining = Vec::new();
        reader.read_to_end(&mut remaining).unwrap();
        assert_eq!(remaining, b"unread");
    }

    #[tokio::test]
    async fn backpressure_resumes_without_reordering_or_duplicating_bytes() {
        let (reader, _writer) = std::io::pipe().unwrap();
        let expected: Vec<_> = (0..250u8).cycle().take(32768).collect();
        let mut source = expected.as_slice();
        let (mut output, mut receiving) = tokio::io::duplex(31);
        let send = async {
            relay(&mut source, &mut output, || owner_closed(&reader))
                .await
                .unwrap();
            output.shutdown().await.unwrap();
        };
        let receive = async {
            let mut actual = Vec::new();
            receiving.read_to_end(&mut actual).await.unwrap();
            actual
        };
        let (_, actual) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(send, receive)
        })
        .await
        .unwrap();
        assert_eq!(actual, expected);
    }
}
