//! The address space (UA-lite): node B describes itself, node A browses it, reads a button,
//! writes a LED and watches the button, all through the lines that would travel on the link.
//!
//! cargo run -p workshop-core --example space --target host-tuple
//!
//! The `// --8<--` markers delimit the parts included in the documentation (docs/book/protocols.md).

use workshop_core::config::NodeConfig;
use workshop_core::space::{self, Body, Frame, NodeSpace, Status, Subscriptions};

/// What the two nodes hold, between requests.
struct NodeB {
    cfg: NodeConfig,
    subs: Subscriptions<4>,
    jumper: bool,
    seq: u32,
}

impl NodeB {
    /// A line arrives from the link: answer it, and return the line to send back.
    fn on_line(&mut self, line: &str, now_ms: u32) -> String {
        let request = space::parse(line.trim_end()).expect("a valid frame");
        let mut node = NodeSpace {
            cfg: &mut self.cfg,
            uptime_ms: now_ms,
            jumper: self.jumper,
            boot: false,
            tx: 8,
            rx: 8,
            lost: 0,
        };
        let body = space::serve(&request, &mut node, &mut self.subs, now_ms).expect("a request");
        self.seq += 1;
        space::encode(&Frame { node: "node-B", seq: self.seq, t_ms: now_ms, body }).to_string()
    }
}

fn main() {
    let mut b = NodeB {
        cfg: NodeConfig { name: "node-B", ..NodeConfig::DEFAULT },
        subs: Subscriptions::new(),
        jumper: false,
        seq: 0,
    };
    let mut seq_a = 0;
    // A sends a request: the line on the wire, then the line B answers with.
    let mut ask = |b: &mut NodeB, body: Body, now_ms: u32| {
        seq_a += 1;
        let request = space::encode(&Frame { node: "node-A", seq: seq_a, t_ms: now_ms, body });
        let answer = b.on_line(&request, now_ms);
        println!("A> {}\nB> {}", request.trim_end(), answer.trim_end());
        space::parse(answer.trim_end()).expect("a valid answer").body
    };

    // --8<-- [start:browse]
    // Browse /leds: ask for child 0, 1, 2... until `more` is false.
    let leds = space::find("/leds").unwrap();
    for index in 0.. {
        let Body::BrowseResult { id, name, more, .. } = ask(&mut b, Body::Browse { id: leds, index }, 1000) else {
            break;
        };
        println!("   found {} (id {id})", name);
        if !more {
            break;
        }
    }
    // --8<-- [end:browse]

    // --8<-- [start:read_write]
    let jumper = space::find("/buttons/jumper").unwrap();
    let Body::Value { value, .. } = ask(&mut b, Body::Read { id: jumper }, 2000) else { panic!() };
    println!("   jumper = {value}");

    let led1 = space::find("/leds/1").unwrap();
    let Body::Value { status, value, .. } =
        ask(&mut b, Body::Write { id: led1, value: "blink 200".try_into().unwrap() }, 3000)
    else {
        panic!()
    };
    println!("   LED 1 written: {status}, now {value}");
    // A value the node refuses leaves the setting alone and says why.
    let Body::Value { status, value, .. } =
        ask(&mut b, Body::Write { id: led1, value: "disco".try_into().unwrap() }, 4000)
    else {
        panic!()
    };
    println!("   LED 1 written: {status}, still {value}");
    assert_eq!(status, Status::BadTypeMismatch);
    // --8<-- [end:read_write]

    // --8<-- [start:watch]
    // Watch the jumper every 100 ms: the answer is its value now...
    ask(&mut b, Body::Subscribe { id: jumper, interval_ms: 100 }, 5000);
    // ...then a frame whenever it changes. B is silent until someone presses the button:
    b.jumper = true;
    let mut node = NodeSpace { cfg: &mut b.cfg, uptime_ms: 5200, jumper: b.jumper, boot: false, tx: 8, rx: 8, lost: 0 };
    if let Some(body) = b.subs.poll(5200, &mut node) {
        let line = space::encode(&Frame { node: "node-B", seq: 99, t_ms: 5200, body });
        println!("B> {}", line.trim_end());
    }
    // --8<-- [end:watch]
}
