//! The daemon's `DownPorts` request releases exactly the listed ports.
mod common;

use common::Harness;
use devkit_ports::daemon::proto::{Request, Response};
use devkit_ports::registry::Role;

#[test]
fn down_ports_releases_listed_reservations() {
    let mut h = Harness::start();

    // The holder must be an existing directory so liveness probes (`holder_alive`)
    // do not prune these pidless reservations before the assertions observe them.
    let holder = h.home.to_str().unwrap().to_string();

    // Two pidless reservations under one holder.
    let alloc = h.request(&Request::Alloc {
        holder,
        reqs: vec![("api".into(), 9100), ("web".into(), 9200)],
        role: Role::Issue,
    });
    let ports: Vec<u16> = match alloc {
        Response::Ports(v) => v.into_iter().map(|(_, p)| p).collect(),
        other => panic!("unexpected alloc response: {other:?}"),
    };
    assert_eq!(ports.len(), 2);

    // Down exactly one of them.
    let resp = h.request(&Request::DownPorts {
        ports: vec![ports[0]],
    });
    match resp {
        Response::Freed(freed) => assert_eq!(freed, vec![ports[0]]),
        other => panic!("unexpected DownPorts response: {other:?}"),
    }

    // The other reservation is still tracked.
    let snap = h.request(&Request::Snapshot);
    match snap {
        Response::Snapshot(d) => {
            assert!(d.entries.contains_key(&ports[1]), "unlisted port survives");
            assert!(!d.entries.contains_key(&ports[0]), "listed port released");
        }
        other => panic!("unexpected snapshot response: {other:?}"),
    }

    h.shutdown();
}
