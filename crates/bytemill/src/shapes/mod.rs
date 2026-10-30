//! Shape extraction package, porting `bps/shapes/__init__.py`.

pub mod extract;
pub mod items;
pub mod ledger;
pub mod model;
pub mod scan;

pub use extract::{crate_root, extract_shape, packet_root};
pub use items::{Item, Member, impl_body_bounds, impl_members, scan_items};
pub use ledger::{
    DerivedField, DerivedKind, KnownDeviation, PairPlan, bml_field_pairs, extract_shapes_for_units,
    hand_packet_path, known_deviations, load_ledger, mismatch, mismatch_fields, module_path, packet_report,
    pair_body_fields, pair_fields,
};
pub use model::{
    Accepted, AliasShape, FieldBind, FieldShape, ItemBind, NewShape, SerializerShape, Shape, ShapeError,
    WriteSpec, shape_from_json, shape_to_json,
};
pub use scan::{advance_item_start, doc_line_text, docs_and_attrs_before};
