use fuse::{Filesystem, MountOpt};
use std::env;

struct NullFS;

impl Filesystem for NullFS {}

fn main() {
    env_logger::init();
    let mountpoint = env::args_os().nth(1).unwrap();
    fuse::mount(NullFS, mountpoint, MountOpt::AllowOther).unwrap();
}
