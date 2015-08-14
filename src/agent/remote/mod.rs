use std::mem::replace;
use std::net::{SocketAddr, SocketAddrV4, Ipv4Addr};
use std::ops::DerefMut;
use std::sync::{Arc, RwLock};
use std::collections::HashMap;

use mio::{Token, Timeout, EventSet, Sender};
use mio::util::Slab;
use probor;
use time::{SteadyTime, Duration};
use rand::thread_rng;
use rand::distributions::{IndependentSample, Range};

use query::{Filter, Extract};
use super::server::Context;
use super::scan::time_ms;
use super::websock::{Beacon, write_binary};
use super::websock::InputMessage as OutputMessage;
use super::websock::OutputMessage as InputMessage;
use super::deps::{LockedDeps};
use super::server::Timer::{ReconnectPeer, ResetPeer};
use super::p2p::GossipStats;
use super::p2p;
use self::owebsock::WebSocket;
use super::history::History;
use super::ioutil::Poll;
use super::server::Timer::{RemoteCollectGarbage};

mod owebsock;
mod aggregate;
pub mod respond;


const SLAB_START: usize = 1000000000;
const MAX_OUTPUT_CONNECTIONS: usize = 4096;
const HANDSHAKE_TIMEOUT: u64 = 30000;
const MESSAGE_TIMEOUT: u64 = 15000;
const GARBAGE_COLLECTOR_INTERVAL: u64 = 60_000;
const SUBSCRIPTION_LIFETIME: i64 = 3 * 60_000;
const DATA_POINTS: usize = 150;  // ~ five minutes ~ 150px of graph
pub const EXTRACT: Extract = Extract::HistoryByNum(DATA_POINTS);
pub const EXTRACT_ONE: Extract = Extract::HistoryByNum(1);  // just latest one


#[allow(unused)] // start_time will be used later
pub struct Peers {
    touch_time: SteadyTime,
    gc_timer: Timeout,
    pub connected: usize,
    pub addresses: HashMap<SocketAddr, Token>,
    pub peers: Slab<Peer>,
    subscriptions: HashMap<Filter, SteadyTime>,
}

pub struct Peer {
    pub addr: SocketAddr,
    connection: Option<WebSocket>,
    timeout: Timeout,
    history: History,
    pub last_beacon: Option<(u64, Beacon)>,
}

impl Peer {
    pub fn connected(&self) -> bool {
        self.connection.as_ref().map(|x| !x.handshake).unwrap_or(false)
    }
}


pub fn ensure_started(ctx: &mut Context) {
    let pref = ctx.deps.get::<Arc<RwLock<Option<Peers>>>>().unwrap().clone();
    let mut opt_peers = pref.write().unwrap();
    if let &mut Some(ref mut peers) = opt_peers.deref_mut() {
        peers.touch_time = SteadyTime::now();
        return; // already started
    }
    debug!("Starting remote tracking");
    let range = Range::new(5, 150);
    let mut rng = thread_rng();
    let peers:Vec<_>;
    peers = ctx.deps.read::<GossipStats>().peers.keys().cloned().collect();
    let mut data = Peers {
        touch_time: SteadyTime::now(),
        peers: Slab::new_starting_at(Token(SLAB_START),
                                     MAX_OUTPUT_CONNECTIONS),
        gc_timer: ctx.eloop.timeout_ms(RemoteCollectGarbage,
            GARBAGE_COLLECTOR_INTERVAL).unwrap(),
        connected: 0,
        addresses: HashMap::new(),
        subscriptions: HashMap::new(),
    };
    for addr in peers {
        if let Some(tok) = data.peers.insert_with(|tok| Peer {
            addr: addr,
            last_beacon: None,
            connection: None,
            history: History::new(),
            timeout: ctx.eloop.timeout_ms(ReconnectPeer(tok),
                range.ind_sample(&mut rng)).unwrap(),
        }) {
            data.addresses.insert(addr, tok);
        } else {
            error!("Too many peers");
        }
    }
    *opt_peers = Some(data);

    ctx.deps.get::<Sender<p2p::Command>>().unwrap()
        .send(p2p::Command::RemoteSwitch(true))
        .map_err(|_| error!("Error sending RemoteSwitch to p2p"))
        .ok();
}

pub fn add_peer(addr: SocketAddr, ctx: &mut Context) {
    debug!("Adding peer {:?}", addr);
    let range = Range::new(5, 150);
    let mut rng = thread_rng();
    let mut opt_peers = ctx.deps.write::<Option<Peers>>();
    if opt_peers.is_none() {
        // Remote handling is not enabled ATM
        return;
    }
    let data = opt_peers.as_mut().unwrap();
    if data.addresses.contains_key(&addr) {
        return;
    }
    let ref mut eloop = ctx.eloop;
    if let Some(tok) = data.peers.insert_with(|tok| Peer {
        addr: addr,
        last_beacon: None,
        connection: None,
        timeout: eloop.timeout_ms(ReconnectPeer(tok),
            range.ind_sample(&mut rng)).unwrap(),
        history: History::new(),
    }) {
        data.addresses.insert(addr, tok);
    } else {
        error!("Too many peers");
    }
}

pub fn reconnect_peer(tok: Token, ctx: &mut Context) {
    let mut peers_opt = ctx.deps.write::<Option<Peers>>();
    let data = peers_opt.as_mut().unwrap();
    if let Some(ref mut peer) = data.peers.get_mut(tok) {
        assert!(peer.connection.is_none());
        let range = Range::new(1000, 2000);
        let mut rng = thread_rng();
        if let Ok(conn) = WebSocket::connect(peer.addr) {
            match conn.register(tok, ctx.eloop) {
                Ok(_) => {
                    peer.connection = Some(conn);
                    peer.timeout = ctx.eloop.timeout_ms(ResetPeer(tok),
                        HANDSHAKE_TIMEOUT).unwrap();
                }
                _ => {
                    peer.connection = None;
                    peer.timeout = ctx.eloop.timeout_ms(ReconnectPeer(tok),
                        range.ind_sample(&mut rng)).unwrap();
                }
            }
        } else {
            peer.connection = None;
            peer.timeout = ctx.eloop.timeout_ms(ReconnectPeer(tok),
                range.ind_sample(&mut rng)).unwrap();
        }
    }
}

pub fn reset_peer(tok: Token, ctx: &mut Context) {
    let mut peers_opt = ctx.deps.write::<Option<Peers>>();
    let data = peers_opt.as_mut().unwrap();
    if let Some(ref mut peer) = data.peers.get_mut(tok) {
        let wsock = replace(&mut peer.connection, None)
            .expect("No socket to reset");
        ctx.eloop.remove(&wsock.sock);
        let range = Range::new(1000, 2000);
        let mut rng = thread_rng();
        peer.timeout = ctx.eloop.timeout_ms(ReconnectPeer(tok),
            range.ind_sample(&mut rng)).unwrap();
    }
}

pub fn try_io(tok: Token, ev: EventSet, ctx: &mut Context) -> bool
{
    let pref = ctx.deps.get::<Arc<RwLock<Option<Peers>>>>().unwrap().clone();
    let mut opt_peers = pref.write().unwrap();
    let data = opt_peers.as_mut().unwrap();
    if let Some(ref mut peer) = data.peers.get_mut(tok) {
        let to_close = {
            let ref mut wsock = peer.connection.as_mut()
                .expect("Can't read from non-existent socket");
            let old = wsock.handshake;
            let mut to_close;
            if let Some(messages) = wsock.events(ev, tok, ctx) {
                if messages.len() > 0 {
                    assert!(ctx.eloop.clear_timeout(peer.timeout));
                    peer.timeout = ctx.eloop.timeout_ms(ResetPeer(tok),
                        MESSAGE_TIMEOUT).unwrap();
                }
                for msg in messages {
                    match msg {
                        InputMessage::Beacon(b) => {
                            peer.last_beacon = Some((time_ms(), b));
                        }
                        InputMessage::NewIPv4Peer(ip, port) => {
                            // TODO(tailhook) process it
                            let ip = Ipv4Addr::from(ip);
                            debug!("New peer from websock {:?}", ip);
                            ctx.deps.get::<Sender<p2p::Command>>().unwrap()
                            .send(p2p::Command::AddGossipHost(
                                  SocketAddr::V4(SocketAddrV4::new(ip, port))))
                            .unwrap()
                        }
                        InputMessage::Stats(stats) => {
                            debug!("New stats from peer {:?}", peer.addr);
                            trace!("Stat values {:?}: {:?}", peer.addr, stats);
                            unimplemented!();
                            //peer.history.update_chunks(stats);
                        }
                    }
                }
                to_close = false;
            } else {
                to_close = true;
            }
            if old &&  !to_close && !wsock.handshake {
                debug!("Connected websocket to {:?}", peer.addr);
                data.connected += 1;
                assert!(ctx.eloop.clear_timeout(peer.timeout));
                peer.timeout = ctx.eloop.timeout_ms(ResetPeer(tok),
                    MESSAGE_TIMEOUT).unwrap();
                if data.subscriptions.len() > 0 {
                    for rule in data.subscriptions.keys() {
                        let subscr = OutputMessage::Subscribe(
                            rule.clone(), DATA_POINTS);
                        let msg = probor::to_buf(&subscr);
                        write_binary(&mut wsock.output, &msg);
                    }
                    ctx.eloop.modify(&wsock.sock, tok, true, true);
                }
            } else if !old && to_close {
                debug!("Disconnected websocket from {:?}", peer.addr);
                data.connected -= 1;
            }
            to_close
        };
        if to_close {
            let range = Range::new(5, 150);
            let mut rng = thread_rng();
            peer.connection = None;
            assert!(ctx.eloop.clear_timeout(peer.timeout));
            peer.timeout = ctx.eloop.timeout_ms(ReconnectPeer(tok),
                    range.ind_sample(&mut rng)).unwrap();
        }
        return true;
    } else {
        return false;
    }
    // unreachable
    //data.peers.remove(tok)
    //return true;
}

pub fn garbage_collector(ctx: &mut Context) {
    debug!("Garbage collector");
    let mut peers_opt = ctx.deps.write::<Option<Peers>>();
    let peers = peers_opt.as_mut().unwrap();

    let cut_off = SteadyTime::now() - Duration::milliseconds(
        SUBSCRIPTION_LIFETIME);
    peers.subscriptions = replace(&mut peers.subscriptions, HashMap::new())
        .into_iter()
        .filter(|&(_, timestamp)| timestamp > cut_off)
        .collect();

    for peer in peers.peers.iter_mut() {
        // TODO(tailhook) Is it ok to truncate by time? Do we want some
        // stale data to be around on ocassion?
        peer.history.truncate_by_time((DATA_POINTS as u64)*2000+2000);
    }

    peers.gc_timer = ctx.eloop.timeout_ms(RemoteCollectGarbage,
        GARBAGE_COLLECTOR_INTERVAL).unwrap();
}
