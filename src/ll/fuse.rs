use libc;
use std::ffi::CString;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::IntoRawFd;

pub fn fuse_mount_sys(mountpoint: *const i8, flags: u64) -> i32 {
    // TODO:Check args
    // TODO:Check mountpoint
    // TODO:Check nonempty
    // TODO:Check auto_umount
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .unwrap();
    // TODO:Check f
    // from:sdcard.c    sprintf(opts, "fd=%i,rootmode=40000,default_permissions,allow_other,"
    //                                "user_id=%d,group_id=%d", fd, uid, gid);
    let opts = format!(
        "fd={},rootmode={},default_permissions,allow_other,user_id={},group_id={}",
        f.as_raw_fd(),
        40000,
        unsafe { libc::getuid() },
        unsafe { libc::getgid() }
    );
    // TODO: Add kernel opt
    //
    //TODO: understand:
    /*
    strcpy(type, mo->blkdev ? "fuseblk" : "fuse");
    if (mo->subtype) {
        strcat(type, ".");
        strcat(type, mo->subtype);
    }
    strcpy(source,
           mo->fsname ? mo->fsname : (mo->subtype ? mo->subtype : devname));
    */
    let c_sources = CString::new("/dev/fuse").unwrap();
    let c_fs = CString::new("fuse").unwrap();
    let c_opts = CString::new(opts).unwrap();
    let res = unsafe {
        #[cfg(target_pointer_width = "32")]
        let flags = flags as u32;
        #[cfg(target_os = "android")]
        let mountpoint = mountpoint as *const u8;
        libc::mount(
            c_sources.as_ptr(),
            mountpoint,
            c_fs.as_ptr(),
            flags,
            c_opts.as_ptr() as *mut libc::c_void,
        )
    };
    if res < 0 {
        res
    } else {
        f.into_raw_fd()
    }
}
