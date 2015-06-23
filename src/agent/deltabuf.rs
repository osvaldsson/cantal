use std::cmp::min;
use std::collections::VecDeque;
use std::collections::vec_deque::Iter as DequeIter;

const SIGN_BIT: u8     = 0b00100000;
const SPECIAL_BIT: u8  = 0b01000000;  // WARNING! check only with CONTINUATION
const SPECIAL_BITS: u8 = 0b11100000;
const SPECIAL_MASK: u8 = 0b00011111;
//                       vv
const SKIP_BITS: u8 = 0b01100000;
const ZERO_BITS: u8 = 0b01000000;
//                       ^^
const FIRST_BYTE_SHIFT: u32 = 5;
const CONTINUATION_BIT: u8 = 0b10000000;
const CONTINUATION_SHIFT: u32 = 7;
const FIRST_BYTE_MASK: u8 = 0b00011111;
const CONTINUATION_MASK: u8 = 0b01111111;

#[derive(RustcDecodable, RustcEncodable, Debug, Clone)]
pub struct DeltaBuf(VecDeque<u8>);

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Delta {
    Positive(u64),
    Negative(u64),
    Skip,
}


#[derive(PartialEq, Eq, Debug, Clone, Copy)]
enum DequeItem {
    Empty,
    Gaps(u8),
    Zeros(u8),
    Diff(Delta),
}

#[derive(Clone)]
pub struct DeltaIter<'a> {
    iter: DequeIter<'a, u8>,
    queue: DequeItem,
}

impl<'a> DeltaIter<'a> {
    fn refill_queue(&mut self) {
        use self::DequeItem::*;

        let mut delta = 0;
        loop {
            let byte = match self.iter.next() {
                Some(x) => *x,
                None => {
                    if delta != 0 { error!("EOF in the middle of delta"); }
                    break;
                }
            };
            if byte & CONTINUATION_BIT != 0 {
                delta <<= CONTINUATION_SHIFT;
                delta |= (byte & CONTINUATION_MASK) as u64;
            } else {
                if byte & SPECIAL_BIT != 0 {
                    if byte & SPECIAL_BITS == SKIP_BITS {
                        let num = byte & SPECIAL_MASK;
                        if num <= 0 { error!("Bad gaps count"); }
                        self.queue = Gaps(num);
                        break;
                    } else if byte & SPECIAL_BITS == ZERO_BITS {
                        let num = byte & SPECIAL_MASK;
                        if num <= 0 { error!("Bad zeros count"); }
                        self.queue = Zeros(num);
                        break;
                    } else {
                        error!("Errorneous special bits");
                    }
                } else {
                    delta <<= FIRST_BYTE_SHIFT;
                    delta |= (byte & FIRST_BYTE_MASK) as u64;
                    if byte & SIGN_BIT != 0 {
                        self.queue = Diff(Delta::Negative(delta));
                    } else {
                        self.queue = Diff(Delta::Positive(delta));
                    }
                    break;
                }
            }
        }
    }
}

impl<'a> Iterator for DeltaIter<'a> {
    type Item = Delta;

    fn next(&mut self) -> Option<Delta> {
        use self::DequeItem::*;
        match self.queue {
            Empty | Gaps(0) | Zeros(0) => self.refill_queue(),
            _ => {}
        }
        let (nque, result) = match self.queue {
            Empty => (Empty, None),
            Diff(x) => (Empty, Some(x)),
            Gaps(1) => (Empty, Some(Delta::Skip)),
            Gaps(x) => (Gaps(x-1), Some(Delta::Skip)),
            Zeros(1) => (Empty, Some(Delta::Positive(0))),
            Zeros(x) => (Zeros(x-1), Some(Delta::Positive(0))),
        };
        self.queue = nque;
        result
    }
}

impl DeltaBuf {
    pub fn new() -> DeltaBuf {
        return DeltaBuf(VecDeque::new());
    }
    pub fn push(&mut self, old_value: i64, new_value: i64, mut age_diff: u64)
    {
        let DeltaBuf(ref mut deque) = *self;
        if age_diff == 0 {
            warn!("Duplicate write at same age"); // Shouldn't we panic?
            return;
        }
        age_diff -= 1;
        while age_diff > 0 {
            let cd = min(age_diff as i64, SPECIAL_MASK as i64) as u8;
            deque.push_front(SKIP_BITS as u8 | cd);
            age_diff -= cd as u64;
        }
        let (mut delta, sign) = if old_value > new_value {
            (old_value - new_value, SIGN_BIT)
        } else {
            (new_value - old_value, 0)
        };
        if delta == 0 {
            if deque.len() > 0 && deque[0] & SPECIAL_BITS == ZERO_BITS {
                let old_val = deque[0] & SPECIAL_MASK;
                if old_val < SPECIAL_MASK {
                    deque[0] = (old_val+1) | ZERO_BITS;
                    return;
                }
            }
            deque.push_front(ZERO_BITS as u8 | 1);
            return;
        }
        deque.push_front(sign | (delta as u8 & FIRST_BYTE_MASK));
        delta = delta >> FIRST_BYTE_SHIFT;
        while delta > From::from(0) {
            deque.push_front((delta as u8 & CONTINUATION_MASK) |
                CONTINUATION_BIT);
            delta = delta >> CONTINUATION_SHIFT;
        }
    }
    pub fn deltas<'a>(&'a self) -> DeltaIter<'a> {
        DeltaIter {
            iter: self.0.iter(),
            queue: DequeItem::Empty,
        }
    }
    pub fn truncate(&mut self, limit: usize) -> usize {
        if limit == 0 {
            *self = DeltaBuf::new();  // Is this efficient?
            return 0;
        }
        match self._truncate_bytes(limit) {
            Ok((limit_bytes, truncate_num)) => {
                let DeltaBuf(ref mut deque) = *self;
                if truncate_num > 0 {
                    let b = deque[limit_bytes-1];
                    debug_assert!(b & CONTINUATION_BIT as u8 == 0);
                    debug_assert!(b & SPECIAL_MASK as u8 > truncate_num as u8);
                    deque[limit_bytes-1] = (b & SPECIAL_BITS as u8) |
                        ((b & SPECIAL_MASK as u8) - truncate_num as u8);
                }
                // TODO(tailhook) use truncate
                while deque.len() > limit_bytes {
                    deque.pop_back();
                }
                // deque.truncate(limit_bytes);
                limit
            }
            Err(num_current) => num_current,
        }
    }
    fn _truncate_bytes(&self, limit: usize) -> Result<(usize, u8), usize> {
        let DeltaBuf(ref deque) = *self;
        let mut counter = 0usize;
        for (idx, &byte) in deque.iter().enumerate() {
            if byte & CONTINUATION_BIT as u8 != 0 {
                continue;
            }
            if byte & SPECIAL_BIT as u8 != 0 {
                let cnt = byte & SPECIAL_MASK as u8;
                let newcnt = counter + cnt as usize;
                if newcnt == limit {
                    return Ok((idx+1, 0));
                } else if newcnt > limit {
                    return Ok((idx+1, cnt - (limit - counter) as u8));
                } else {
                    counter = newcnt;
                }
            } else {
                counter += 1;
                if counter >= limit {
                    return Ok((idx+1, 0));
                }
            }
        }
        return Err(counter);
    }
}


#[cfg(test)]
mod test {

    use std::fmt::Display;
    use super::{Delta, DeltaBuf};
    use super::Delta::*;

    fn to_buf(values: &[i64]) -> DeltaBuf {
        let mut buf = DeltaBuf::new();
        for idx in 0..(values.len()-1) {
            buf.push(values[idx], values[idx+1], 1);
        }
        return buf;
    }
    fn to_buf_opt(values: &[Option<i64>])
        -> DeltaBuf
    {
        let mut buf = DeltaBuf::new();
        let mut off = 0;
        let mut old = values[0].unwrap();
        for idx in 0..(values.len()-1) {
            off += 1;
            values[idx+1].map(|v| {
                buf.push(old, v, off);
                old = v;
                off = 0;
            });
        }
        return buf;
    }

    fn deltify(values: &[i64]) -> Vec<Delta> {
        let buf = to_buf(values);
        println!("BUFFER {:?}", buf);
        return buf.deltas().collect()
    }
    fn deltify_opt(values: &[Option<i64>])
        -> Vec<Delta>
    {
        return to_buf_opt(values).deltas().collect()
    }

    #[test]
    fn i64_no_skips() {
        assert_eq!(deltify(&[1, 2, 10, 1000, 100000, 5, 5, 5, 5, 10]),
            vec!(Positive(5), Positive(0), Positive(0), Positive(0),
                 Negative(99995), Positive(99000),
                 Positive(990), Positive(8), Positive(1) ));
    }
    #[test]
    fn i64_zero_cont_bug() {
        assert_eq!(deltify(&[0, 2943, 2943, 2943]),
            vec!(Positive(0), Positive(0), Positive(2943)));
    }
    #[test]
    fn i64_skips() {
        assert_eq!(deltify_opt(&[Some(1), Some(2), None, Some(10),
                                 Some(1000), None, None, None, None,
                                 Some(100000), Some(5), Some(10)]),
            vec!(Positive(5), Negative(99995), Positive(99000),
                 Skip, Skip, Skip, Skip,
                 Positive(990), Positive(8), Skip, Positive(1) ));
    }

    #[test]
    fn i64_partial_read() {
        let buf = to_buf_opt(&[Some(1), Some(2), None, Some(10),
                               Some(1000), None, None, None, None,
                               Some(100000), Some(5), Some(10)]);
        let result = vec!(Positive(5), Negative(99995), Positive(99000),
                          Skip, Skip, Skip, Skip,
                          Positive(990), Positive(8), Skip, Positive(1));
        for i in 0..result.len() {
            assert_eq!(&buf.deltas().take(i).collect::<Vec<_>>()[..],
                       &result[..i]);
        }
    }

    #[test]
    fn i64_truncate() {
        let buf = to_buf_opt(&[Some(1), Some(2), None, Some(10),
                               Some(1000), None, None, None, None,
                               Some(100000), Some(5), Some(10)]);
        let result = vec!(Positive(5), Negative(99995), Positive(99000),
                          Skip, Skip, Skip, Skip,
                          Positive(990), Positive(8), Skip, Positive(1));
        for i in 0..result.len() {
            let mut b = buf.clone();
            assert_eq!(b.truncate(i), i);
            assert_eq!(&b.deltas().collect::<Vec<_>>()[..], &result[..i]);
        }
        let mut b = buf.clone();
        assert_eq!(b.deltas().count(), 11);
        assert_eq!(b.truncate(100), 11);
        assert_eq!(b.deltas().collect::<Vec<_>>(), result);
    }
}
