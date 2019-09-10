use std::ffi::CString;
use std::fmt;
use std::fs::OpenOptions;
use std::io;
use std::ops;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::IntoRawFd;
use std::path::Path;

pub enum MountOpt {
    Fd(i32),
    RootMode(u32),
    DefaultPermissions,
    AllowOther,
    UID(u32),
    GID(u32),
    List(Vec<MountOpt>),
    Name(&'static str),
}

macro_rules! param_contains {
    ($self: expr, $var:path) => {
        match $self {
            $var(_) => true,
            MountOpt::List(ls) => {
                ls.iter()
                    .filter(|e| if let $var(_) = e { true } else { false })
                    .count()
                    > 0
            }
            _ => false,
        }
    };
}

impl MountOpt {
    ///
    /// Set required options to default if it was no present
    ///
    pub fn missing_default(mut self) -> Self {
        if !param_contains!(&self, MountOpt::UID) {
            self = self + MountOpt::UID(unsafe { libc::getuid() });
        }
        if !param_contains!(&self, MountOpt::GID) {
            self = self + MountOpt::GID(unsafe { libc::getgid() });
        }
        if !param_contains!(&self, MountOpt::RootMode) {
            self = self + MountOpt::RootMode(40755);
        }
        self
    }
}

impl fmt::Display for MountOpt {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MountOpt::Fd(e) => write!(fmt, "fd={}", e),
            MountOpt::Name(e) => write!(fmt, "fsname={}", e),
            MountOpt::RootMode(e) => write!(fmt, "rootmode={}", e),
            MountOpt::DefaultPermissions => write!(fmt, "default_permissions"),
            MountOpt::AllowOther => write!(fmt, "allow_other"),
            MountOpt::UID(e) => write!(fmt, "user_id={}", e),
            MountOpt::GID(e) => write!(fmt, "group_id={}", e),
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
pub fn fuse_mount<T: AsRef<Path>>(mountpoint: T, args: MountOpt) -> Result<i32, io::Error> {
    // TODO: check if allow_other and allow_root aren't mutually active
    let fuse_fd = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .unwrap();
    // Set uid, gid and root mode if they are missing from parametters and set fd
    let args = MountOpt::Fd(fuse_fd.as_raw_fd()) + args.missing_default();
    let args = format!("{}", args);
    let c_sources = CString::new("/dev/fuse").unwrap();
    let c_fs = CString::new("fuse").unwrap();
    let c_opts = CString::new(args).unwrap();
    let mountpoint = CString::new(mountpoint.as_ref().to_str().unwrap()).unwrap();
    println!("{:?} {:?}", &c_opts, &mountpoint);
    #[cfg(target_os = "android")]
    let mountpoint = mountpoint.as_ptr() as *const u8;
    #[cfg(not(target_os = "android"))]
    let mountpoint = mountpoint.as_ptr() as *const i8;
    let res = unsafe {
        libc::mount(
            c_sources.as_ptr(),
            mountpoint,
            c_fs.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            c_opts.as_ptr() as *mut libc::c_void,
        )
    };
    if res < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fuse_fd.into_raw_fd())
    }
}
