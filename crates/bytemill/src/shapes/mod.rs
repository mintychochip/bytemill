//! Shape extraction package, porting `bps/shapes/__init__.py`.

pub mod extract;
pub mod items;
pub mod ledger;
pub mod model;
pub mod scan;

pub use extract::{extract_shape, PACKET_ROOT};
pub use items::{impl_body_bounds, impl_members, scan_items, Item, Member};
pub use ledger::{
    bps_field_pairs, extract_shapes_for_units, hand_packet_path, load_ledger, mismatch,
    module_path, packet_report,
};
pub use model::{
    shape_from_json, shape_to_json, Accepted, FieldShape, NewShape, SerializerShape, Shape,
    ShapeError,
};
pub use scan::{advance_item_start, doc_line_text, docs_and_attrs_before};
