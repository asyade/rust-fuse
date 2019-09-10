//! Filesystem operation request
//!
//! A request represents information about a filesystem operation the kernel driver wants us to
//! perform.
//!
//! TODO: This module is meant to go away soon in favor of `ll::Request`.

use fuse_abi::consts::*;
use fuse_abi::*;
use libc::{EIO, ENOSYS, EPROTO};
use log::{debug, error, warn};
use std::convert::TryFrom;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::channel::ChannelSender;
use crate::ll;
use crate::reply::{Reply, ReplyDirectory, ReplyEmpty, ReplyRaw};
use crate::session::MAX_WRITE_SIZE;
use crate::Filesystem;

/// We generally support async reads
#[cfg(not(target_os = "macos"))]
const INIT_FLAGS: u32 = FUSE_ASYNC_READ;
// TODO: Add FUSE_EXPORT_SUPPORT and FUSE_BIG_WRITES (requires ABI 7.10)

/// On macOS, we additionally support case insensitiveness, volume renames and xtimes
/// TODO: we should eventually let the filesystem implementation decide which flags to set
#[cfg(target_os = "macos")]
const INIT_FLAGS: u32 = FUSE_ASYNC_READ | FUSE_CASE_INSENSITIVE | FUSE_VOL_RENAME | FUSE_XTIMES;
// TODO: Add FUSE_EXPORT_SUPPORT and FUSE_BIG_WRITES (requires ABI 7.10)

/// Request data structure
#[derive(Debug)]
pub struct Request<'a> {
    /// Channel sender for sending the reply
    ch: ChannelSender,
    /// Request raw data
    data: &'a [u8],
    /// Parsed request
    request: ll::Request<'a>,
}

///
/// Something that can hanlde a fuse request
///
pub trait RequestDispatcher {
    ///
    /// Dispatch a fuse Reques on the filesystem and save proto/state into the session store
    ///
    fn dispatch(&mut self, request: &mut Request<'_>, se: &mut super::session::FuseSessionStore);
}

impl<T: Filesystem> RequestDispatcher for T {
    fn dispatch(&mut self, request: &mut Request<'_>, se: &mut super::session::FuseSessionStore) {
        debug!("{}", request.request);
        match request.request.operation() {
            // Filesystem initialization
            ll::Operation::Init { arg } => {
                let reply: ReplyRaw<fuse_init_out> = request.reply();
                // We don't support ABI versions before 7.6
                if arg.major < 7 || (arg.major == 7 && arg.minor < 6) {
                    error!("Unsupported FUSE ABI version {}.{}", arg.major, arg.minor);
                    reply.error(EPROTO);
                    return;
                }
                // Remember ABI version supported by kernel
                se.proto_major = arg.major;
                se.proto_minor = arg.minor;
                // Call filesystem init method and give it a chance to return an error
                let res = self.init(request);
                if let Err(err) = res {
                    reply.error(err);
                    return;
                }
                // Reply with our desired version and settings. If the kernel supports a
                // larger major version, it'll re-send a matching init message. If it
                // supports only lower major versions, we replied with an error above.
                let init = fuse_init_out {
                    major: FUSE_KERNEL_VERSION,
                    minor: FUSE_KERNEL_MINOR_VERSION,
                    max_readahead: arg.max_readahead, // accept any readahead size
                    flags: arg.flags & INIT_FLAGS, // use features given in INIT_FLAGS and reported as capable
                    unused: 0,
                    max_write: MAX_WRITE_SIZE as u32, // use a max write size that fits into the session's buffer
                };
                debug!(
                    "INIT response: ABI {}.{}, flags {:#x}, max readahead {}, max write {}",
                    init.major, init.minor, init.flags, init.max_readahead, init.max_write
                );
                se.initialized = true;
                reply.ok(&init);
            }
            // Any operation is invalid before initialization
            _ if !se.initialized => {
                warn!("Ignoring FUSE operation before init: {}", request.request);
                request.reply::<ReplyEmpty>().error(EIO);
            }
            // Filesystem destroyed
            ll::Operation::Destroy => {
                self.destroy(request);
                se.destroyed = true;
                request.reply::<ReplyEmpty>().ok();
            }
            // Any operation is invalid after destroy
            _ if se.destroyed => {
                warn!("Ignoring FUSE operation after destroy: {}", request.request);
                request.reply::<ReplyEmpty>().error(EIO);
            }

            ll::Operation::Interrupt { .. } => {
                // TODO: handle FUSE_INTERRUPT
                request.reply::<ReplyEmpty>().error(ENOSYS);
            }

            ll::Operation::Lookup { name } => {
                self.lookup(request, request.request.nodeid(), &name, request.reply());
            }
            ll::Operation::Forget { arg } => {
                self.forget(request, request.request.nodeid(), arg.nlookup); // no reply
            }
            ll::Operation::GetAttr => {
                self.getattr(request, request.request.nodeid(), request.reply());
            }
            ll::Operation::SetAttr { arg } => {
                let mode = match arg.valid & FATTR_MODE {
                    0 => None,
                    _ => Some(arg.mode),
                };
                let uid = match arg.valid & FATTR_UID {
                    0 => None,
                    _ => Some(arg.uid),
                };
                let gid = match arg.valid & FATTR_GID {
                    0 => None,
                    _ => Some(arg.gid),
                };
                let size = match arg.valid & FATTR_SIZE {
                    0 => None,
                    _ => Some(arg.size),
                };
                let atime = match arg.valid & FATTR_ATIME {
                    0 => None,
                    _ => Some(UNIX_EPOCH + Duration::new(arg.atime, arg.atimensec)),
                };
                let mtime = match arg.valid & FATTR_MTIME {
                    0 => None,
                    _ => Some(UNIX_EPOCH + Duration::new(arg.mtime, arg.mtimensec)),
                };
                let fh = match arg.valid & FATTR_FH {
                    0 => None,
                    _ => Some(arg.fh),
                };
                #[cfg(target_os = "macos")]
                #[inline]
                fn get_macos_setattr(
                    arg: &fuse_setattr_in,
                ) -> (
                    Option<SystemTime>,
                    Option<SystemTime>,
                    Option<SystemTime>,
                    Option<u32>,
                ) {
                    let crtime = match arg.valid & FATTR_CRTIME {
                        0 => None,
                        _ => Some(UNIX_EPOCH + Duration::new(arg.crtime, arg.crtimensec)),
                    };
                    let chgtime = match arg.valid & FATTR_CHGTIME {
                        0 => None,
                        _ => Some(UNIX_EPOCH + Duration::new(arg.chgtime, arg.chgtimensec)),
                    };
                    let bkuptime = match arg.valid & FATTR_BKUPTIME {
                        0 => None,
                        _ => Some(UNIX_EPOCH + Duration::new(arg.bkuptime, arg.bkuptimensec)),
                    };
                    let flags = match arg.valid & FATTR_FLAGS {
                        0 => None,
                        _ => Some(arg.flags),
                    };
                    (crtime, chgtime, bkuptime, flags)
                }
                #[cfg(not(target_os = "macos"))]
                #[inline]
                fn get_macos_setattr(
                    _arg: &fuse_setattr_in,
                ) -> (
                    Option<SystemTime>,
                    Option<SystemTime>,
                    Option<SystemTime>,
                    Option<u32>,
                ) {
                    (None, None, None, None)
                }
                let (crtime, chgtime, bkuptime, flags) = get_macos_setattr(arg);
                self.setattr(
                    request,
                    request.request.nodeid(),
                    mode,
                    uid,
                    gid,
                    size,
                    atime,
                    mtime,
                    fh,
                    crtime,
                    chgtime,
                    bkuptime,
                    flags,
                    request.reply(),
                );
            }
            ll::Operation::ReadLink => {
                self.readlink(request, request.request.nodeid(), request.reply());
            }
            ll::Operation::MkNod { arg, name } => {
                self.mknod(
                    request,
                    request.request.nodeid(),
                    &name,
                    arg.mode,
                    arg.rdev,
                    request.reply(),
                );
            }
            ll::Operation::MkDir { arg, name } => {
                self.mkdir(
                    request,
                    request.request.nodeid(),
                    &name,
                    arg.mode,
                    request.reply(),
                );
            }
            ll::Operation::Unlink { name } => {
                self.unlink(request, request.request.nodeid(), &name, request.reply());
            }
            ll::Operation::RmDir { name } => {
                self.rmdir(request, request.request.nodeid(), &name, request.reply());
            }
            ll::Operation::SymLink { name, link } => {
                self.symlink(
                    request,
                    request.request.nodeid(),
                    &name,
                    &Path::new(link),
                    request.reply(),
                );
            }
            ll::Operation::Rename { arg, name, newname } => {
                self.rename(
                    request,
                    request.request.nodeid(),
                    &name,
                    arg.newdir,
                    &newname,
                    request.reply(),
                );
            }
            ll::Operation::Link { arg, name } => {
                self.link(
                    request,
                    arg.oldnodeid,
                    request.request.nodeid(),
                    &name,
                    request.reply(),
                );
            }
            ll::Operation::Open { arg } => {
                self.open(
                    request,
                    request.request.nodeid(),
                    arg.flags,
                    request.reply(),
                );
            }
            ll::Operation::Read { arg } => {
                self.read(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    arg.offset as i64,
                    arg.size,
                    request.reply(),
                );
            }
            ll::Operation::Write { arg, data } => {
                assert!(data.len() == arg.size as usize);
                self.write(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    arg.offset as i64,
                    data,
                    arg.write_flags,
                    request.reply(),
                );
            }
            ll::Operation::Flush { arg } => {
                self.flush(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    arg.lock_owner,
                    request.reply(),
                );
            }
            ll::Operation::Release { arg } => {
                let flush = match arg.release_flags & FUSE_RELEASE_FLUSH {
                    0 => false,
                    _ => true,
                };
                self.release(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    arg.flags,
                    arg.lock_owner,
                    flush,
                    request.reply(),
                );
            }
            ll::Operation::FSync { arg } => {
                let datasync = match arg.fsync_flags & 1 {
                    0 => false,
                    _ => true,
                };
                self.fsync(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    datasync,
                    request.reply(),
                );
            }
            ll::Operation::OpenDir { arg } => {
                self.opendir(
                    request,
                    request.request.nodeid(),
                    arg.flags,
                    request.reply(),
                );
            }
            ll::Operation::ReadDir { arg } => {
                self.readdir(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    arg.offset as i64,
                    ReplyDirectory::new(request.request.unique(), request.ch, arg.size as usize),
                );
            }
            ll::Operation::ReleaseDir { arg } => {
                self.releasedir(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    arg.flags,
                    request.reply(),
                );
            }
            ll::Operation::FSyncDir { arg } => {
                let datasync = match arg.fsync_flags & 1 {
                    0 => false,
                    _ => true,
                };
                self.fsyncdir(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    datasync,
                    request.reply(),
                );
            }
            ll::Operation::StatFs => {
                self.statfs(request, request.request.nodeid(), request.reply());
            }
            ll::Operation::SetXAttr { arg, name, value } => {
                assert!(value.len() == arg.size as usize);
                #[cfg(target_os = "macos")]
                #[inline]
                fn get_position(arg: &fuse_setxattr_in) -> u32 {
                    arg.position
                }
                #[cfg(not(target_os = "macos"))]
                #[inline]
                fn get_position(_arg: &fuse_setxattr_in) -> u32 {
                    0
                }
                self.setxattr(
                    request,
                    request.request.nodeid(),
                    name,
                    value,
                    arg.flags,
                    get_position(arg),
                    request.reply(),
                );
            }
            ll::Operation::GetXAttr { arg, name } => {
                self.getxattr(
                    request,
                    request.request.nodeid(),
                    name,
                    arg.size,
                    request.reply(),
                );
            }
            ll::Operation::ListXAttr { arg } => {
                self.listxattr(request, request.request.nodeid(), arg.size, request.reply());
            }
            ll::Operation::RemoveXAttr { name } => {
                self.removexattr(request, request.request.nodeid(), name, request.reply());
            }
            ll::Operation::Access { arg } => {
                self.access(request, request.request.nodeid(), arg.mask, request.reply());
            }
            ll::Operation::Create { arg, name } => {
                self.create(
                    request,
                    request.request.nodeid(),
                    &name,
                    arg.mode,
                    arg.flags,
                    request.reply(),
                );
            }
            ll::Operation::GetLk { arg } => {
                self.getlk(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    arg.owner,
                    arg.lk.start,
                    arg.lk.end,
                    arg.lk.typ,
                    arg.lk.pid,
                    request.reply(),
                );
            }
            ll::Operation::SetLk { arg } => {
                self.setlk(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    arg.owner,
                    arg.lk.start,
                    arg.lk.end,
                    arg.lk.typ,
                    arg.lk.pid,
                    false,
                    request.reply(),
                );
            }
            ll::Operation::SetLkW { arg } => {
                self.setlk(
                    request,
                    request.request.nodeid(),
                    arg.fh,
                    arg.owner,
                    arg.lk.start,
                    arg.lk.end,
                    arg.lk.typ,
                    arg.lk.pid,
                    true,
                    request.reply(),
                );
            }
            ll::Operation::BMap { arg } => {
                self.bmap(
                    request,
                    request.request.nodeid(),
                    arg.blocksize,
                    arg.block,
                    request.reply(),
                );
            }

            #[cfg(target_os = "macos")]
            ll::Operation::SetVolName { name } => {
                self.setvolname(request, name, request.reply());
            }
            #[cfg(target_os = "macos")]
            ll::Operation::GetXTimes => {
                self.getxtimes(request, request.request.nodeid(), request.reply());
            }
            #[cfg(target_os = "macos")]
            ll::Operation::Exchange {
                arg,
                oldname,
                newname,
            } => {
                self.exchange(
                    request,
                    arg.olddir,
                    &oldname,
                    arg.newdir,
                    &newname,
                    arg.options,
                    request.reply(),
                );
            }
        }
    }
}

impl<'a> Request<'a> {
    /// Create a new request from the given data
    pub fn new(ch: ChannelSender, data: &'a [u8]) -> Option<Request<'a>> {
        let request = match ll::Request::try_from(data) {
            Ok(request) => request,
            Err(err) => {
                // FIXME: Reply with ENOSYS?
                error!("{}", err);
                return None;
            }
        };

        Some(Self { ch, data, request })
    }

    /// Create a reply object for this request that can be passed to the filesystem
    /// implementation and makes sure that a request is replied exactly once
    fn reply<T: Reply>(&self) -> T {
        Reply::new(self.request.unique(), self.ch)
    }

    /// Returns the unique identifier of this request
    #[inline]
    #[allow(dead_code)]
    pub fn unique(&self) -> u64 {
        self.request.unique()
    }

    /// Returns the uid of this request
    #[inline]
    #[allow(dead_code)]
    pub fn uid(&self) -> u32 {
        self.request.uid()
    }

    /// Returns the gid of this request
    #[inline]
    #[allow(dead_code)]
    pub fn gid(&self) -> u32 {
        self.request.gid()
    }

    /// Returns the pid of this request
    #[inline]
    #[allow(dead_code)]
    pub fn pid(&self) -> u32 {
        self.request.pid()
    }
}
