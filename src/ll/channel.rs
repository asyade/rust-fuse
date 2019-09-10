use libc;
use libc::c_int;
use std::io;

fn get_fd_flags(fd: c_int) -> c_int {
    unsafe { libc::fcntl(fd, libc::F_GETFL) }
}

fn set_fd_flags(fd: c_int, flags: c_int) -> c_int {
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags) }
}

pub fn set_nonblocking(fd: c_int, nonblocking: bool) -> io::Result<()> {
    // Taken from https://github.com/rust-lang/rust/blob/6ccfe68076abc78392ab9e1d81b5c1a2123af657/src/libstd/sys/unix/fd.rs#L164
    // Behavior should be consistent accross OSes.
    match get_fd_flags(fd) {
        -1 => Err(io::Error::last_os_error()),
        previous => {
            let new = if nonblocking {
                previous | libc::O_NONBLOCK
            } else {
                previous & !libc::O_NONBLOCK
            };
            if new != previous && set_fd_flags(fd, new) == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}
