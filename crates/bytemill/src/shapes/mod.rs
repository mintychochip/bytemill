//! Shape extraction package, porting `bps/shapes/__init__.py`.

pub mod extract;
pub mod items;
pub mod ledger;
pub mod model;
pub mod scan;

pub use extract::{PACKET_ROOT, extract_shape};
pub use items::{Item, Member, impl_body_bounds, impl_members, scan_items};
pub use ledger::{
    bml_field_pairs, extract_shapes_for_units, hand_packet_path, load_ledger, mismatch,
    module_path, packet_report, pair_fields,
};
pub use model::{
    Accepted, FieldShape, NewShape, SerializerShape, Shape, ShapeError, shape_from_json,
    shape_to_json,
};
pub use scan::{advance_item_start, doc_line_text, docs_and_attrs_before};
