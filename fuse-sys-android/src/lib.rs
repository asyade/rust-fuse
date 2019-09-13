//! Native FFI bindings to libfuse.
//!
//! This is a small set of bindings that are required to mount/unmount FUSE filesystems and
//! open/close a fd to the FUSE kernel driver.
#![feature(rustc_private)]
extern crate libc;

use {
    libc::{mount, umount2, MNT_DETACH, MS_NOATIME, MS_NODEV, MS_NOEXEC, MS_NOSUID},
    std::{
        ffi::{c_void, CStr, CString, NulError, OsStr},
        fs::OpenOptions,
        os::{
            raw::c_int,
            unix::io::{AsRawFd, IntoRawFd},
        },
    },
};

macro_rules! try_or_raw {
    ($e:expr, $raw: expr) => {
        if let Ok(res) = $e {
            res
        } else {
            return $raw;
        }
    };
}

fn args_with_fd(fd: c_int, args: &[&OsStr]) -> Result<CString, NulError> {
    let mut fmt = format!("fd={}", fd);
    for elem in args.iter() {
        fmt += &format!(
            ",{}",
            elem.to_str()
                .expect("Fuse argument contains null character")
        );
    }
    CString::new(fmt)
}

///
/// On Android there is not libfuse or fusermount so we need to manualy get a FD from the fuse
/// device and then use it as to mount the mp using libc::mount, this require root priviliges
/// anyway you need it to run a FUSE filesystem on android
///
/// Example: on android >7 default parametters are are : `rootmode=40000,allow_other,user_id=9997,group_id=999`
///
pub unsafe fn fuse_mount_android_compat<C: AsRef<CStr>>(mountpoint: C, args: &[&OsStr]) -> c_int {
    let fuse_fd = try_or_raw!(
        OpenOptions::new().read(true).write(true).open("/dev/fuse"),
        -1
    );
    let device = CString::new("/dev/fuse").expect("CString::new failed");
    let name = CString::new("fuse").expect("CString::new failed");
    let options = try_or_raw!(args_with_fd(fuse_fd.as_raw_fd(), args), -2);
    umount2(mountpoint.as_ref().as_ptr(), MNT_DETACH);
    if mount(
        device.as_ptr(),
        mountpoint.as_ref().as_ptr(),
        name.as_ptr(),
        MS_NOSUID | MS_NODEV | MS_NOEXEC | MS_NOATIME,
        options.as_ptr() as *mut c_void,
    ) < 0
    {
        return -3;
    }
    fuse_fd.into_raw_fd()
}
