use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;

/// A file descriptor that turns readable once any thread, or a signal handler writing to
/// [`Wakeup::writer`], wakes it, and stays readable until cleared.
#[derive(Debug)]
pub struct Wakeup {
    reader: UnixStream,
    writer: UnixStream,
}

impl Wakeup {
    pub fn new() -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok(Self { reader, writer })
    }

    /// Makes the descriptor readable. A full socket buffer already is, so a failed write is moot.
    pub fn wake(&self) {
        let _ = (&self.writer).write(&[1]);
    }

    /// Makes the descriptor unreadable again, until the next wake.
    pub fn clear(&self) {
        let mut sink = [0; 64];
        while (&self.reader).read(&mut sink).is_ok_and(|read| read > 0) {}
    }

    /// Another handle to the writing end, for code that wakes through a raw descriptor.
    pub fn writer(&self) -> io::Result<UnixStream> {
        self.writer.try_clone()
    }
}

impl AsFd for Wakeup {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.reader.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    use super::*;

    fn is_readable(wakeup: &Wakeup) -> io::Result<bool> {
        let mut fds = [PollFd::new(wakeup, PollFlags::IN)];
        Ok(poll(&mut fds, Some(&Timespec::default()))? > 0)
    }

    #[test]
    fn stays_readable_from_wake_until_clear() -> io::Result<()> {
        let wakeup = Wakeup::new()?;
        assert!(!is_readable(&wakeup)?);
        wakeup.wake();
        wakeup.wake();
        assert!(is_readable(&wakeup)?);
        wakeup.clear();
        assert!(!is_readable(&wakeup)?);
        wakeup.writer()?.write_all(&[1])?;
        assert!(is_readable(&wakeup)?);
        Ok(())
    }
}
