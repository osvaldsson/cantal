use std::default::Default;
use std::str::FromStr;
use std::fs::File;
use std::io::{BufReader, Read, BufRead};
use cantal::itertools::NextValue;


use super::{time_ms};
use super::Tip;
use super::super::stats::Key;
use cantal::Value::{Float, Counter, Integer};


pub fn read(t: &mut Tip) -> Option<u64> {
    let mut boot_time = None::<u64>;
    File::open(&Path::new("/proc/uptime"))
        .and_then(|mut f| {
            let mut buf = String::with_capacity(100);
            f.read_to_string(&mut buf)
            .map(|_| buf)})
        .map(|buf| {
            let mut pieces = buf.words();
            // TODO(tailhook) they are float counters?
            t.add_next_float("uptime", &mut pieces);
            t.add_next_float("idle_time", &mut pieces);
        }).ok();
    File::open(&Path::new("/proc/loadavg"))
        .and_then(|mut f| {
            let mut buf = String::with_capacity(100);
            f.read_to_string(&mut buf)
            .map(|_| buf)
        })
        .map(|buf| {
            let mut pieces = buf.words();
            t.add_next_float("load_avg_1min", &mut pieces);
            t.add_next_float("load_avg_5min", &mut pieces);
            t.add_next_float("load_avg_15min", &mut pieces);
            let mut proc_pieces = pieces.next()
                .map(|x| x.splitn(1, '/'))
                .map(|mut p| {
                    t.add_next_cnt("proc_runnable", &mut pieces);
                    t.add_next_cnt("proc_total", &mut pieces);
                });
            t.add_next_float("last_pid", &mut pieces);
        }).ok();

    File::open(&Path::new("/proc/stat"))
        .and_then(|f| {
            let mut f = BufReader::new(f);
            loop {
                let mut line = String::with_capacity(100);
                try!(f.read_line(&mut line));
                if line.len() == 0 { break; }
                if line.starts_with("cpu ") {
                    let mut pieces = line.words();
                    pieces.next();
                    t.add_next_cnt("cpu.user", &mut pieces);
                    t.add_next_cnt("cpu.nice", &mut pieces);
                    t.add_next_cnt("cpu.system", &mut pieces);
                    t.add_next_cnt("cpu.idle", &mut pieces);
                    t.add_next_cnt("cpu.iowait", &mut pieces);
                    t.add_next_cnt("cpu.irq", &mut pieces);
                    t.add_next_cnt("cpu.softirq", &mut pieces);
                    t.add_next_cnt("cpu.steal", &mut pieces);
                    t.add_next_cnt("cpu.guest", &mut pieces);
                    t.add_next_cnt("cpu.guest_nice", &mut pieces);
                } else if line.starts_with("btime ") {
                    boot_time = FromStr::from_str(line[6..].trim()).ok();
                }
            }
            Ok(())
        }).ok();
    File::open(&Path::new("/proc/meminfo"))
        .and_then(|f| {
            let mut f = BufReader::new(f);
            loop {
                let mut line = String::with_capacity(50);
                try!(f.read_line(&mut line));
                if line.len() == 0 { break; }
                let mut pieces = line.words();
                let ksuffix = if let Some(x) = pieces.next() { x }
                    else { continue; };
                let key = Key::metric(&format!("memory.{}",
                                     ksuffix.trim_right_matches(':')));
                let val = match pieces.next() {
                    Some(val) => val,
                    None => continue,
                };
                let mult = match pieces.next() {
                    Some("kB") => 1024,
                    Some(x) => {
                        debug!("Unknown memory unit {:?}", x);
                        continue;
                    }
                    None => 1,
                };
                FromStr::from_str(val).map(|x| t.add(key, Integer(x*mult)));
            }
            Ok(())
        }).ok();
    File::open(&Path::new("/proc/net/dev"))
        .and_then(|f| {
            let mut f = BufReader::new(f);
            let mut line = String::with_capacity(200);
            try!(f.read_line(&mut line));
            let mut line = String::with_capacity(200);
            try!(f.read_line(&mut line));
            let mut slices = line.splitn(2, '|');
            slices.next();
            let mut fields = vec!();
            for i in slices.next().unwrap_or("").words() {
                fields.push(format!("net.interface.rx.{}", i));
            }
            for i in slices.next().unwrap_or("").words() {
                fields.push(format!("net.interface.tx.{}", i));
            }
            loop {
                let mut line = String::with_capacity(200);
                try!(f.read_line(&mut line));
                if line.len() == 0 { break; }
                let mut pieces = line.words();
                let interface = pieces.next().unwrap_or("unknown:")
                                .trim_right_matches(':');
                for (k, v) in fields.iter().zip(pieces) {
                    FromStr::from_str(v).map(|x|
                        t.add(
                            Key::pairs(&[
                                ("interface", interface),
                                ("metric", &k),
                                ]),
                            Counter(x)))
                    .ok();
                }
            }
            Ok(())
        }).ok();
    File::open(&Path::new("/proc/net/netstat"))
    .and_then(|f| {
        let mut f = BufReader::new(f);
        loop {
            let mut header_line = String::with_capacity(2048);
            try!(f.read_line(&mut header_line));
            if header_line.len() == 0 { break; }
            let mut header = header_line.words();

            let mut values_line = String::with_capacity(1024);
            try!(f.read_line(&mut values_line));
            if values_line.len() == 0 { break; }
            let mut values = values_line.words();

            let first = header.next();
            if first != values.next() {
                break;
            }
            let prefix = first.unwrap().trim_right_matches(':');
            for (k, v) in header.zip(values) {
                FromStr::from_str(v).map(|x|
                    t.add(
                        Key::metric(&format!("net.{}.{}", prefix, k)),
                        Counter(x)))
                .ok();
            }
        }
        Ok(())
    }).ok();
    return boot_time;
}
