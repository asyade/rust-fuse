use fuse::{
    FileAttr, FileType, Filesystem, MountOpt, RecvResult, ReplyAttr, ReplyData, ReplyDirectory,
    ReplyEntry, Request,
};
use libc::ENOENT;
use mio::{Events, Poll, PollOpt, Ready, Token};
use std::env;
use std::ffi::OsStr;
use std::time::{Duration, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(1); // 1 second

const HELLO_DIR_ATTR: FileAttr = FileAttr {
    ino: 1,
    size: 0,
    blocks: 0,
    atime: UNIX_EPOCH, // 1970-01-01 00:00:00
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::Directory,
    perm: 0o755,
    nlink: 2,
    uid: 501,
    gid: 20,
    rdev: 0,
    flags: 0,
};

const HELLO_TXT_CONTENT: &str = "Hello World!\n";

const HELLO_TXT_ATTR: FileAttr = FileAttr {
    ino: 2,
    size: 13,
    blocks: 1,
    atime: UNIX_EPOCH, // 1970-01-01 00:00:00
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::RegularFile,
    perm: 0o644,
    nlink: 1,
    uid: 501,
    gid: 20,
    rdev: 0,
    flags: 0,
};

struct HelloFS;

impl Filesystem for HelloFS {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent == 1 && name.to_str() == Some("hello.txt") {
            reply.entry(&TTL, &HELLO_TXT_ATTR, 0);
        } else {
            reply.error(ENOENT);
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        match ino {
            1 => reply.attr(&TTL, &HELLO_DIR_ATTR),
            2 => reply.attr(&TTL, &HELLO_TXT_ATTR),
            _ => reply.error(ENOENT),
        }
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        _size: u32,
        reply: ReplyData,
    ) {
        if ino == 2 {
            reply.data(&HELLO_TXT_CONTENT.as_bytes()[offset as usize..]);
        } else {
            reply.error(ENOENT);
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if ino != 1 {
            reply.error(ENOENT);
            return;
        }

        let entries = vec![
            (1, FileType::Directory, "."),
            (1, FileType::Directory, ".."),
            (2, FileType::RegularFile, "hello.txt"),
        ];

        for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
            // i + 1 means the index of the next entry
            reply.add(entry.0, (i + 1) as i64, entry.1, entry.2);
        }
        reply.ok();
    }
}

fn main() -> std::io::Result<()> {
    env_logger::init();
    let mountpoint = env::args_os().nth(1).unwrap();
    let mut session = fuse::evented(
        mountpoint,
        MountOpt::Name("evented") + MountOpt::DefaultPermissions + MountOpt::AllowOther,
    )
    .unwrap();
    let poll = Poll::new().unwrap();
    let mut buffer: Vec<u8> = Vec::with_capacity((16 * 1024 * 1024) + 4096);
    let mut events = Events::with_capacity(1024);
    poll.register(&session, Token(1), Ready::readable(), PollOpt::level())
        .unwrap();
    loop {
        poll.poll(&mut events, None).unwrap();
        for e in events.iter().filter(|evt| !evt.readiness().is_empty()) {
            'read_until_eagin: loop {
                match session.recv(&mut buffer) {
                    RecvResult::Some(request) => {}
                    RecvResult::Retry => break 'read_until_eagin,
                    RecvResult::Drop(e) => {
                        eprintln!("Fs must be deregistred: {:?}", e);
                        poll.deregister(&session)?;
                    }
                }
            }
        }
    }
    Ok(())
}
