use std::rc::Rc;
use std::str::FromStr;
use std::ffi::OsStr;
use std::os::unix::prelude::OsStrExt;
use std::io::{BufReader, BufRead, Read};
use std::str::from_utf8;
use std::hash::{Hash};
use std::path::{Path, PathBuf, Component};
use std::fs::{File, read_dir};
use std::collections::{HashMap};
use std::collections::hash_map::Entry::{Occupied, Vacant};
use rustc_serialize::json::Json;
use libc;

use cantal::{Metadata, MetadataError, Descriptor, Value};
use cantal::find_elem;
use cantal::itertools::{NextValue, NextStr, words};
use cantal::iotools::{ReadHostBytes};
use super::super::mountpoints::{MountPrefix, parse_mount_point};

pub type Pid = u32;

pub struct ReadCache {
    tick: u32,
}

#[derive(RustcEncodable)]
pub struct MinimalProcess {
    pub pid: Pid,
    pub ppid: Pid,
    pub name: String,
    pub state: char,
    pub vsize: u64,
    pub rss: u64,
    pub num_threads: u32,
    pub start_time: u64,
    pub user_time: u32,
    pub system_time: u32,
    pub child_user_time: u32,
    pub child_system_time: u32,
    pub cmdline: String,
}

fn page_size() -> usize {
    // TODO(tailhook) use env::page_size when that's stabilized
    return 4096;
}

fn read_process(cache: &mut ReadCache, pid: Pid)
    -> Result<MinimalProcess, ()>
{
    let cmdline = try!(File::open(&format!("/proc/{}/cmdline", pid))
        .and_then(|mut f| f.read_chunk(4096))
        .map_err(|e| debug!("Can't read cmdline file")));
    // Command-line may be non-full, but we don't care
    let cmdline = String::from_utf8_lossy(&cmdline);

    let buf = try!(File::open(&format!("/proc/{}/stat", pid))
        .and_then(|mut f| f.read_chunk(4096))
        .map_err(|e| debug!("Can't read stat file: {}", e)));
    if buf.len() >= 4096 {
        error!("Stat line too long");
        return Err(());
    }

    let name_start = try!(find_elem(&buf, &b'(').ok_or(()));
    // Since there might be brackets in the name itself we should use last
    // closing paren
    let name_end = try!(find_elem(&buf, &b')').ok_or(()));
    let name = try!(from_utf8(&buf[name_start+1..name_end])
        .map_err(|e| debug!("Can't decode stat file: {}", e)))
        .to_string();

    let stat_line = try!(from_utf8(&buf[name_end+1..])
        .map_err(|e| debug!("Can't decode stat file: {}", e)));
    let mut words = words(&stat_line);


    return Ok(MinimalProcess {
        pid: pid,
        name: name,
        state: try!(words.next_str()).chars().next().unwrap_or('-'),
        ppid: try!(words.next_value()),
        user_time: try!(words.nth_value(9)),
        system_time: try!(words.next_value()),
        child_user_time: try!(words.next_value()),
        child_system_time: try!(words.next_value()),
        num_threads: try!(words.nth_value(2)),
        start_time: {
            let stime: u64 = try!(words.nth_value(1));
            (stime * 1000) / cache.tick as u64 },
        vsize: try!(words.next_value()),
        rss: {
            let rss: u64 = try!(words.next_value());
            rss * page_size() as u64},
        cmdline: cmdline.to_string(),
    });
}

pub fn read(cache: &mut ReadCache) -> Vec<MinimalProcess> {
    read_dir("/proc")
    .map_err(|e| error!("Error listing /proc: {}", e))
    .map(|lst| lst
        .filter_map(|x| x.ok())
        .filter_map(|x| x.path().file_name()
                         .and_then(|x| x.to_str())
                         .and_then(|x| FromStr::from_str(x).ok()))
        .filter_map(|x| read_process(cache, x).ok())
        .collect())
    .unwrap_or(Vec::new())
}

impl ReadCache {
    pub fn new() -> ReadCache {
        ReadCache {
            tick: unsafe {
                libc::sysconf(libc::consts::os::sysconf::_SC_CLK_TCK) as u32
            },
        }
    }
}
