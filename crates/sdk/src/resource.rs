//! Networks move integer quantities between blocks. Machines produce and consume the resource.

use crate::{Component, Record, Registry, Tag, Text, WithComponents};

/// A storage component shared by the machines on one channel.
pub use crate::abi::Energy as Storage;

static CONDUIT: Component<Text> = Component::named("core:resource_conduit");
static STORAGE: Component<Text> = Component::named("core:resource_storage");
static UNIT: Component<Text> = Component::named("core:resource_unit");

pub static LIQUID_CONDUIT: Tag = Tag::named("core:liquid_conduit");
pub const LITRE: &str = "unit.litre";

/// Translation key for a channel's unit.
pub fn unit_key(channel: &Record) -> Option<&'static str> {
    UNIT.on(crate::abi::entry_kind::RECORD, channel.id())
        .map(|text| text.as_str())
}

/// Register one kind of resource. Each storage component belongs to one channel and is
/// attached to the machine blocks. Conduit blocks carry the supplied tag. Several channels
/// can share conduits, for example water and lava without mixing their inventories.
pub fn register(r: &mut Registry, channel: &'static Record, conduit: &Tag, storage: &Component<Storage>, unit_key: &str) {
    r.instance_component(storage, Storage::default(), crate::abi::component_flags::SAVE | crate::abi::component_flags::SYNC_MENU);
    r.record(channel)
        .set_text(&CONDUIT, conduit.name())
        .set_text(&STORAGE, storage.name())
        .set_text(&UNIT, unit_key);
}
