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
use crate::session::{Session, MAX_WRITE_SIZE};
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

    /// Dispatch request to the given filesystem.
    /// This calls the appropriate filesystem operation method for the
    /// request and sends back the returned reply to the kernel
    pub fn dispatch<SE: super::FuseSession, FS: Filesystem>(
        &self,
        se: &mut SE,
        fs: &mut Filesystem,
    ) {
        debug!("{}", self.request);
        match self.request.operation() {
            // Filesystem initialization
            ll::Operation::Init { arg } => {
                let reply: ReplyRaw<fuse_init_out> = self.reply();
                // We don't support ABI versions before 7.6
                if arg.major < 7 || (arg.major == 7 && arg.minor < 6) {
                    error!("Unsupported FUSE ABI version {}.{}", arg.major, arg.minor);
                    reply.error(EPROTO);
                    return;
                }
                // Remember ABI version supported by kernel
                se.set_proto_major(arg.major);
                se.set_proto_minor(arg.minor);
                // Call filesystem init method and give it a chance to return an error
                let res = fs.init(self);
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
                se.set_initialized(true);
                reply.ok(&init);
            }
            // Any operation is invalid before initialization
            _ if !se.initialized() => {
                warn!("Ignoring FUSE operation before init: {}", self.request);
                self.reply::<ReplyEmpty>().error(EIO);
            }
            // Filesystem destroyed
            ll::Operation::Destroy => {
                fs.destroy(self);
                se.set_destroyed(true);
                self.reply::<ReplyEmpty>().ok();
            }
            // Any operation is invalid after destroy
            _ if se.destroyed() => {
                warn!("Ignoring FUSE operation after destroy: {}", self.request);
                self.reply::<ReplyEmpty>().error(EIO);
            }

            ll::Operation::Interrupt { .. } => {
                // TODO: handle FUSE_INTERRUPT
                self.reply::<ReplyEmpty>().error(ENOSYS);
            }

            ll::Operation::Lookup { name } => {
                fs.lookup(self, self.request.nodeid(), &name, self.reply());
            }
            ll::Operation::Forget { arg } => {
                fs.forget(self, self.request.nodeid(), arg.nlookup); // no reply
            }
            ll::Operation::GetAttr => {
                fs.getattr(self, self.request.nodeid(), self.reply());
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
                fs.setattr(
                    self,
                    self.request.nodeid(),
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
                    self.reply(),
                );
            }
            ll::Operation::ReadLink => {
                fs.readlink(self, self.request.nodeid(), self.reply());
            }
            ll::Operation::MkNod { arg, name } => {
                fs.mknod(
                    self,
                    self.request.nodeid(),
                    &name,
                    arg.mode,
                    arg.rdev,
                    self.reply(),
                );
            }
            ll::Operation::MkDir { arg, name } => {
                fs.mkdir(self, self.request.nodeid(), &name, arg.mode, self.reply());
            }
            ll::Operation::Unlink { name } => {
                fs.unlink(self, self.request.nodeid(), &name, self.reply());
            }
            ll::Operation::RmDir { name } => {
                fs.rmdir(self, self.request.nodeid(), &name, self.reply());
            }
            ll::Operation::SymLink { name, link } => {
                fs.symlink(
                    self,
                    self.request.nodeid(),
                    &name,
                    &Path::new(link),
                    self.reply(),
                );
            }
            ll::Operation::Rename { arg, name, newname } => {
                fs.rename(
                    self,
                    self.request.nodeid(),
                    &name,
                    arg.newdir,
                    &newname,
                    self.reply(),
                );
            }
            ll::Operation::Link { arg, name } => {
                fs.link(
                    self,
                    arg.oldnodeid,
                    self.request.nodeid(),
                    &name,
                    self.reply(),
                );
            }
            ll::Operation::Open { arg } => {
                fs.open(self, self.request.nodeid(), arg.flags, self.reply());
            }
            ll::Operation::Read { arg } => {
                fs.read(
                    self,
                    self.request.nodeid(),
                    arg.fh,
                    arg.offset as i64,
                    arg.size,
                    self.reply(),
                );
            }
            ll::Operation::Write { arg, data } => {
                assert!(data.len() == arg.size as usize);
                fs.write(
                    self,
                    self.request.nodeid(),
                    arg.fh,
                    arg.offset as i64,
                    data,
                    arg.write_flags,
                    self.reply(),
                );
            }
            ll::Operation::Flush { arg } => {
                fs.flush(
                    self,
                    self.request.nodeid(),
                    arg.fh,
                    arg.lock_owner,
                    self.reply(),
                );
            }
            ll::Operation::Release { arg } => {
                let flush = match arg.release_flags & FUSE_RELEASE_FLUSH {
                    0 => false,
                    _ => true,
                };
                fs.release(
                    self,
                    self.request.nodeid(),
                    arg.fh,
                    arg.flags,
                    arg.lock_owner,
                    flush,
                    self.reply(),
                );
            }
            ll::Operation::FSync { arg } => {
                let datasync = match arg.fsync_flags & 1 {
                    0 => false,
                    _ => true,
                };
                fs.fsync(self, self.request.nodeid(), arg.fh, datasync, self.reply());
            }
            ll::Operation::OpenDir { arg } => {
                fs.opendir(self, self.request.nodeid(), arg.flags, self.reply());
            }
            ll::Operation::ReadDir { arg } => {
                fs.readdir(
                    self,
                    self.request.nodeid(),
                    arg.fh,
                    arg.offset as i64,
                    ReplyDirectory::new(self.request.unique(), self.ch, arg.size as usize),
                );
            }
            ll::Operation::ReleaseDir { arg } => {
                fs.releasedir(self, self.request.nodeid(), arg.fh, arg.flags, self.reply());
            }
            ll::Operation::FSyncDir { arg } => {
                let datasync = match arg.fsync_flags & 1 {
                    0 => false,
                    _ => true,
                };
                fs.fsyncdir(self, self.request.nodeid(), arg.fh, datasync, self.reply());
            }
            ll::Operation::StatFs => {
                fs.statfs(self, self.request.nodeid(), self.reply());
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
                fs.setxattr(
                    self,
                    self.request.nodeid(),
                    name,
                    value,
                    arg.flags,
                    get_position(arg),
                    self.reply(),
                );
            }
            ll::Operation::GetXAttr { arg, name } => {
                fs.getxattr(self, self.request.nodeid(), name, arg.size, self.reply());
            }
            ll::Operation::ListXAttr { arg } => {
                fs.listxattr(self, self.request.nodeid(), arg.size, self.reply());
            }
            ll::Operation::RemoveXAttr { name } => {
                fs.removexattr(self, self.request.nodeid(), name, self.reply());
            }
            ll::Operation::Access { arg } => {
                fs.access(self, self.request.nodeid(), arg.mask, self.reply());
            }
            ll::Operation::Create { arg, name } => {
                fs.create(
                    self,
                    self.request.nodeid(),
                    &name,
                    arg.mode,
                    arg.flags,
                    self.reply(),
                );
            }
            ll::Operation::GetLk { arg } => {
                fs.getlk(
                    self,
                    self.request.nodeid(),
                    arg.fh,
                    arg.owner,
                    arg.lk.start,
                    arg.lk.end,
                    arg.lk.typ,
                    arg.lk.pid,
                    self.reply(),
                );
            }
            ll::Operation::SetLk { arg } => {
                fs.setlk(
                    self,
                    self.request.nodeid(),
                    arg.fh,
                    arg.owner,
                    arg.lk.start,
                    arg.lk.end,
                    arg.lk.typ,
                    arg.lk.pid,
                    false,
                    self.reply(),
                );
            }
            ll::Operation::SetLkW { arg } => {
                fs.setlk(
                    self,
                    self.request.nodeid(),
                    arg.fh,
                    arg.owner,
                    arg.lk.start,
                    arg.lk.end,
                    arg.lk.typ,
                    arg.lk.pid,
                    true,
                    self.reply(),
                );
            }
            ll::Operation::BMap { arg } => {
                fs.bmap(
                    self,
                    self.request.nodeid(),
                    arg.blocksize,
                    arg.block,
                    self.reply(),
                );
            }

            #[cfg(target_os = "macos")]
            ll::Operation::SetVolName { name } => {
                fs.setvolname(self, name, self.reply());
            }
            #[cfg(target_os = "macos")]
            ll::Operation::GetXTimes => {
                fs.getxtimes(self, self.request.nodeid(), self.reply());
            }
            #[cfg(target_os = "macos")]
            ll::Operation::Exchange {
                arg,
                oldname,
                newname,
            } => {
                fs.exchange(
                    self,
                    arg.olddir,
                    &oldname,
                    arg.newdir,
                    &newname,
                    arg.options,
                    self.reply(),
                );
            }
        }
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
