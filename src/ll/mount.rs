use libc::c_int;
use log::{error, info};
use sendfd::UnixSendFd;
use std::ffi::{CStr, CString};
use std::fmt;
use std::fs::OpenOptions;
use std::io;
use std::ops;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub enum MountOpt {
    Raw(String),
    Nosuid,
    Noexec,
    Noatime,
    Nodev,
    AllowOther,
    DefaultPermissions,
    AutoUnmount,
    NonEmpty,
    Fd(i32),
    RootMode(u32),
    UID(u32),
    GID(u32),
    // FSUID(u32),
    // FSGID(u32),
    List(Vec<MountOpt>),
    /// Set the name of the filesystem (wont work on Android)
    Name(&'static str),
}

impl fmt::Display for MountOpt {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MountOpt::Raw(raw) => write!(fmt, "{}", raw),
            MountOpt::Nosuid => write!(fmt, "nosuid"),
            MountOpt::Noexec => write!(fmt, "noexec"),
            MountOpt::Noatime => write!(fmt, "noatime"),
            MountOpt::Nodev => write!(fmt, "nodev"),
            MountOpt::DefaultPermissions => write!(fmt, "default_permissions"),
            MountOpt::AllowOther => write!(fmt, "allow_other"),
            MountOpt::AutoUnmount => write!(fmt, "auto_unmount"),
            MountOpt::NonEmpty => write!(fmt, "nonempty"),
            MountOpt::Fd(e) => write!(fmt, "fd={}", e),
            MountOpt::Name(e) => write!(fmt, "fsname={}", e),
            MountOpt::RootMode(e) => write!(fmt, "rootmode={}", e),
            MountOpt::UID(e) => write!(fmt, "user_id={}", e),
            MountOpt::GID(e) => write!(fmt, "group_id={}", e),
            // MountOpt::FSGID(e) => write!(fmt, "fsgid={}", e),
            // MountOpt::FSUID(e) => write!(fmt, "fsuid={}", e),
            MountOpt::List(lst) => {
                let total = lst.len();
                for (index, param) in lst.iter().enumerate() {
                    write!(fmt, "{}{}", param, if index < total - 1 { "," } else { "" })?;
                }
                Ok(())
            }
        }
    }
}

impl ops::Add<MountOpt> for MountOpt {
    type Output = MountOpt;

    fn add(self, add: Self) -> Self {
        match self {
            MountOpt::List(mut vec) => {
                match add {
                    MountOpt::List(mut list_add) => vec.append(&mut list_add),
                    add => vec.push(add),
                }
                MountOpt::List(vec)
            }
            e => MountOpt::List(vec![e]) + add,
        }
    }
}

#[cfg(not(target_os = "android"))]
pub fn mount<T: AsRef<Path>>(mountpoint: T, args: MountOpt) -> Result<i32, io::Error> {
    fn fuse_mount_fuser<T: AsRef<Path>>(mountpoint: T, args: MountOpt) -> Result<i32, io::Error> {
        let (sock1, sock2) = UnixStream::pair()?;
        if unsafe { libc::fcntl(sock2.as_raw_fd(), libc::F_SETFD, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Command::new("/usr/bin/fusermount")
            .arg("-o")
            .arg(format!("{}", args))
            .arg("--")
            .arg(mountpoint.as_ref())
            .env("_FUSE_COMMFD", format!("{}", sock2.as_raw_fd()))
            .stdout(Stdio::inherit())
            .spawn()?;
        sock1.recvfd().map(|e| e as i32)
    }
    match fuse_mount_fuser(mountpoint, args) {
        Ok(e) => Ok(e),
        Err(e) => {
            dbg!(&e);
            Err(e)
        }
    }
}

#[cfg(target_os = "android")]
pub fn mount<T: AsRef<Path>>(mountpoint: T, args: MountOpt) -> Result<i32, io::Error> {
    fn fuse_mount_sys<T: AsRef<Path>>(mountpoint: T, args: MountOpt) -> Result<i32, io::Error> {
        // TODO: check if allow_other and allow_root aren't mutually active
        let fuse_fd = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/fuse")?;
        // Set uid, gid and root mode if they are missing from parametters and set fd
        let c_sources = CString::new("/dev/fuse")?;
        let c_fs = CString::new("fuse")?;
        let c_opts = CString::new(format!("{}", MountOpt::Fd(fuse_fd.as_raw_fd()) + args))?;
        let mountpoint = CString::new(mountpoint.as_ref().to_str().unwrap()).unwrap();
        println!("{:?} {:?}", &c_opts, &mountpoint);
        #[cfg(target_os = "android")]
        let mountpoint = mountpoint.as_ptr() as *const u8;
        #[cfg(not(target_os = "android"))]
        let mountpoint = mountpoint.as_ptr() as *const i8;
        if unsafe {
            libc::mount(
                c_sources.as_ptr(),
                mountpoint,
                c_fs.as_ptr(),
                libc::MS_NOSUID | libc::MS_NODEV,
                c_opts.as_ptr() as *mut libc::c_void,
            )
        } < 0
        {
            println!("Failed");
            Err(io::Error::last_os_error())
        } else {
            println!("Success: {:?}", fuse_fd);
            Ok(fuse_fd.into_raw_fd())
        }
    }
    let mountpoint = &mountpoint.as_ref().canonicalize().unwrap();
    let re = fuse_mount_sys(&mountpoint, args.clone());
    match &re {
        // Not connected generally means that an dead mountpoint still in use so try to umount it and retry mount
        Err(e) if e.kind() == io::ErrorKind::NotConnected => {
            unmount(mountpoint)?;
            fuse_mount_sys(mountpoint, args)
        }
        _ => re,
    }
}

/// Unmount an arbitrary mount point
pub fn unmount<P: AsRef<Path>>(mountpoint: P) -> io::Result<()> {
    // fuse_unmount_compat22 unfortunately doesn't return a status. Additionally,
    // it attempts to call realpath, which in turn calls into the filesystem. So
    // if the filesystem returns an error, the unmount does not take place, with
    // no indication of the error available to the caller. So we call unmount
    // directly, which is what osxfuse does anyway, since we already converted
    // to the real path when we first mounted.

    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd",
        target_os = "bitrig",
        target_os = "netbsd"
    ))]
    #[inline]
    fn libc_umount(mnt: &CStr) -> c_int {
        unsafe { libc::unmount(mnt.as_ptr(), 0) }
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd",
        target_os = "bitrig",
        target_os = "netbsd"
    )))]
    #[inline]
    fn libc_umount(mnt: &CStr) -> c_int {
        use std::io::ErrorKind::PermissionDenied;

        let rc = unsafe { libc::umount(mnt.as_ptr()) };
        if rc < 0 && io::Error::last_os_error().kind() == PermissionDenied {
            // Linux always returns EPERM for non-root users.  We have to let the
            // library go through the setuid-root "fusermount -u" to unmount.
            unsafe {
                unimplemented!()
                // fuse_unmount_compat22(mnt.as_ptr());
            }
        // 0
        } else {
            rc
        }
    }

    let mnt = CString::new(mountpoint.as_ref().as_os_str().as_bytes())?;
    let rc = libc_umount(&mnt);
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
