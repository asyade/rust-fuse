//! Filesystem session
//!
//! A session runs a filesystem implementation while it is being mounted to a specific mount
//! point. A session begins by mounting the filesystem and ends by unmounting it. While the
//! filesystem is mounted, the session loop receives, dispatches and replies to kernel requests
//! for filesystem operations under its mount point.

use crate::channel::{self, Channel, RecvResult};
use crate::request::Request;
use crate::request::RequestDispatcher;
use crate::Filesystem;
use libc::{EAGAIN, EINTR, ENODEV, ENOENT};
use log::{error, info};
use mio::unix::EventedFd;
use mio::{Evented, Poll, PollOpt, Ready, Token};
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// The max size of write requests from the kernel. The absolute minimum is 4k,
/// FUSE recommends at least 128k, max 16M. The FUSE default is 16M on macOS
/// and 128k on other systems.
pub const MAX_WRITE_SIZE: usize = 16 * 1024 * 1024;

/// Size of the buffer for reading a request from the kernel. Since the kernel may send
/// up to MAX_WRITE_SIZE bytes in a write request, we use that value plus some extra space.
const BUFFER_SIZE: usize = MAX_WRITE_SIZE + 4096;

#[derive(Clone, Debug)]
pub struct FuseSessionStore {
    /// FUSE protocol major version
    pub proto_major: u32,
    /// FUSE protocol minor version
    pub proto_minor: u32,
    /// True if the filesystem is initialized (init operation done)
    pub initialized: bool,
    /// True if the filesystem was destroyed (destroy operation done)
    pub destroyed: bool,
}

impl FuseSessionStore {
    fn new() -> Self {
        Self {
            proto_major: 0,
            proto_minor: 0,
            initialized: false,
            destroyed: false,
        }
    }
}

/// The session data structure
#[derive(Debug)]
pub struct Session<FS: RequestDispatcher> {
    filesystem: FS,
    /// Filesystem operation implementations
    ch: Channel,
    store: FuseSessionStore,
}

impl<FS: Filesystem> Session<FS> {
    /// Create a new session by mounting the given filesystem to the given mountpoint
    pub fn new(filesystem: FS, mountpoint: &Path, options: &str) -> io::Result<Session<FS>> {
        Channel::new(mountpoint, options).map(|ch| Session {
            ch,
            filesystem,
            store: FuseSessionStore::new(),
        })
    }

    /// Return path of the mounted filesystem
    pub fn mountpoint(&self) -> &Path {
        &self.ch.mountpoint()
    }

    /// Run the session loop that receives kernel requests and dispatches them to method
    /// calls into the filesystem. This read-dispatch-loop is non-concurrent to prevent
    /// having multiple buffers (which take up much memory), but the filesystem methods
    /// may run concurrent by spawning threads.
    pub fn run(&mut self) -> io::Result<()> {
        // Buffer for receiving requests from the kernel. Only one is allocated and
        // it is reused immediately after dispatching to conserve memory and allocations.
        let mut buffer: Vec<u8> = Vec::with_capacity(BUFFER_SIZE);
        loop {
            // Read the next request from the given channel to kernel driver
            // The kernel driver makes sure that we get exactly one request per read
            match self.ch.receive_request(&mut buffer) {
                RecvResult::Some(mut request) => {
                    self.filesystem.dispatch(&mut request, &mut self.store)
                }
                RecvResult::Retry => continue,
                RecvResult::Drop(None) => return Ok(()),
                RecvResult::Drop(Some(err)) => return Err(err),
            }
        }
    }
}

///
/// A FuseEvented provides a way to use the FUSE filesystem in a custom event
/// loop. It implements the mio Evented trait, so it can be polled for
/// readiness.
///
// TODO: Drop
#[derive(Debug)]
pub struct EventedSession {
    /// Communication channel to the kernel driver
    ch: Channel,
    pub store: FuseSessionStore,
}

impl Evented for EventedSession {
    fn register(
        &self,
        poll: &Poll,
        token: Token,
        interest: Ready,
        opts: PollOpt,
    ) -> io::Result<()> {
        let raw_fd = unsafe { self.ch.raw_fd() };
        EventedFd(&raw_fd).register(poll, token, interest, opts)
    }
    fn reregister(
        &self,
        poll: &Poll,
        token: Token,
        interest: Ready,
        opts: PollOpt,
    ) -> io::Result<()> {
        let raw_fd = unsafe { self.ch.raw_fd() };
        EventedFd(&raw_fd).reregister(poll, token, interest, opts)
    }
    fn deregister(&self, poll: &Poll) -> io::Result<()> {
        let raw_fd = unsafe { self.ch.raw_fd() };
        EventedFd(&raw_fd).deregister(poll)
    }
}

impl EventedSession {
    ///
    /// Read a request from the fuse fd and process it with the filesystem
    ///
    pub fn new(mountpoint: &Path, options: &str) -> io::Result<Self> {
        Channel::new(mountpoint, options).map(|ch| EventedSession {
            ch,
            store: FuseSessionStore::new(),
        })
    }

    pub fn recv<'a>(&mut self, buffer: &'a mut Vec<u8>) -> RecvResult<'a> {
        self.ch.receive_request(buffer)
    }
}
