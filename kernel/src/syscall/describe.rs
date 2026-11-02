//! The self-describing part: JSON for the kernel and for any handle,
//! generated from the same `sprout-abi` tables userspace links against.
//! An MCP bridge can turn these straight into tool definitions.

use alloc::string::String;

use sprout_abi::{Handle, ObjectType, SYSCALLS, Syscall};

use crate::arch;
use crate::json::Json;
use crate::obj::Object;
use crate::obj::handle::Entry;

fn syscall_json(j: &mut Json<String>, s: &Syscall) {
    j.push_object().num("num", s.num).str("name", s.name);
    j.array("args");
    for a in s.args {
        j.push_object().str("name", a.name).str("type", a.ty).end();
    }
    j.end();
    j.str("returns", s.returns);
    if let Some(object) = s.object {
        j.str("object", object.name());
    }
    j.array("rights");
    for r in s.rights.names() {
        j.push_str(r);
    }
    j.end();
    j.str("doc", s.doc).end();
}

/// What the kernel is and every syscall it answers to.
pub fn kernel() -> String {
    let mut j = Json::new(String::new());
    j.push_object()
        .str("object", "kernel")
        .str("name", "sprout")
        .str("version", crate::VERSION)
        .str("arch", arch::NAME);
    j.array("object_types");
    for t in ObjectType::ALL {
        j.push_str(t.name());
    }
    j.end();
    j.array("syscalls");
    for s in SYSCALLS {
        syscall_json(&mut j, s);
    }
    j.end();
    j.end();
    j.into_inner()
}

/// What one handle refers to, what the holder may do with it, and which
/// operations apply to that kind of object.
pub fn object(handle: Handle, entry: &Entry) -> String {
    let kind = entry.object.kind();
    let mut j = Json::new(String::new());
    j.push_object()
        .str("object", kind.name())
        .num("handle", handle);
    j.array("rights");
    for r in entry.rights.names() {
        j.push_str(r);
    }
    j.end();
    match &entry.object {
        Object::Process(p) => {
            j.num("pid", p.pid)
                .str("name", &p.name)
                .bool("exited", p.exit_code().is_some())
                .num("mapped_bytes", p.mapped_bytes());
        }
        Object::Vmo(v) => {
            let v = v.lock();
            j.num("size", v.size())
                .bool("contiguous", v.is_contiguous());
        }
        Object::Channel(c) => {
            j.num("pending", c.pending())
                .bool("peer_closed", c.peer_closed());
        }
        Object::Port(p) => {
            j.num("attachments", p.attachment_count());
        }
        Object::Device(d) => {
            j.str("kind", d.kind)
                .hex("base", d.base)
                .num("size", d.size)
                .num("irq", d.irq)
                .num("virtio_id", d.virtio_id)
                .str(
                    "virtio_device",
                    crate::obj::device::virtio_device_name(d.virtio_id),
                )
                .num("pending_irqs", d.pending_irqs());
        }
    }
    j.array("ops");
    for s in SYSCALLS.iter().filter(|s| s.object == Some(kind)) {
        j.push_object()
            .num("num", s.num)
            .str("name", s.name)
            .bool("allowed", entry.rights.contains(s.rights))
            .end();
    }
    j.end();
    j.end();
    j.into_inner()
}

crate::ktest! {
    fn kernel_description_lists_every_syscall() {
        let text = kernel();
        assert!(text.starts_with(r#"{"object":"kernel""#));
        for s in SYSCALLS {
            let needle = alloc::format!(r#""name":"{}""#, s.name);
            assert!(text.contains(&needle), "missing {}", s.name);
        }
        assert!(text.contains(
            r#""object_types":["process","thread","vmo","address_space","channel","port","device"]"#
        ));
    }
}

crate::ktest! {
    fn vmo_description_reflects_rights() {
        use crate::mm::vmo::Vmo;
        use crate::sync::SpinLock;
        use alloc::sync::Arc;
        use sprout_abi::Rights;

        let vmo = Object::Vmo(Arc::new(SpinLock::new(Vmo::new(8192).expect("vmo"))));
        let read_only = Entry {
            object: vmo,
            rights: Rights::READ | Rights::DESCRIBE,
        };
        let text = object(7, &read_only);
        assert!(text.contains(r#""object":"vmo","handle":7,"rights":["read","describe"],"size":8192"#));
        assert!(text.contains(r#""name":"vmo_read","allowed":true"#));
        assert!(text.contains(r#""name":"vmo_write","allowed":false"#));
    }
}

crate::ktest! {
    fn channel_description_shows_queue_state() {
        use crate::obj::channel::{Endpoint, Message};
        use sprout_abi::Rights;

        let (a, b) = Endpoint::pair();
        a.send(Message {
            bytes: alloc::vec![1, 2, 3],
            handles: alloc::vec::Vec::new(),
        })
        .expect("send");
        drop(a);
        let entry = Entry {
            object: Object::Channel(b),
            rights: Rights::ALL,
        };
        let text = object(3, &entry);
        assert!(text.contains(r#""object":"channel","handle":3"#));
        assert!(text.contains(r#""pending":1,"peer_closed":true"#));
        assert!(text.contains(r#""name":"channel_recv","allowed":true"#));
    }
}
