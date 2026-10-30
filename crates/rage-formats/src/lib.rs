//! RAGE resource formats: the RSC7-wrapped files GTA V streams — textures
//! (`.ytd`), drawables (`.ydr`/`.ydd`), fragments (`.yft`), archetype and
//! ped metadata (`.ytyp`/`.ymt`), collision (`.ybn`), navmeshes (`.ynv`), path nodes (`.ynd`),
//! texture-dictionary relationships (`gtxd.ymt`), the world cache (`cache_y.dat`) — parsed from bytes into plain Rust structs. Getting those
//! bytes out of an `.rpf` is the `rpf-archive` crate's job; drawing them is
//! `rage-render`'s.

pub mod hash;
pub mod math;
pub mod resource;
pub mod blocks;
pub mod vertex;
pub mod ytd;
pub mod ycd;
pub mod ycd_xml;
pub mod ycd_write;
pub mod gen9;
pub mod ydd;
pub mod yft;
pub mod ymt;
pub mod ytyp;
pub mod gtxd;
pub mod meta_read;
pub mod peds;
pub mod vehicles;
pub mod carcols;
pub mod carvariations;
pub mod carmodcols;
pub mod ybn;
pub mod ymap;
pub mod ynv;
pub mod ynd;
pub mod cache_dat;
pub mod heightmap;
pub mod water;
pub mod value;
pub mod pso;
pub mod meta_schema;
pub mod names;
pub mod xml;
pub mod json;
pub mod ymf;
pub mod texture_utils;
pub mod dds;
mod coerce;
pub mod schema;
pub mod meta_write;
pub mod pso_write;
#[cfg(feature = "encode")]
pub mod texture_encode;
mod rbf;

pub use hash::rage_joaat;
pub use blocks::ydr::{read_ydr, write_ydr, dump_ydr_xml, build_ydr_from_xml, build_ydr_from_xml_checked};
pub use blocks::yft_xml::{dump_yft_xml, build_yft_from_xml, build_yft_from_xml_checked};
pub use blocks::ybn::{read_ybn, write_ybn, dump_ybn_xml, build_ybn_from_xml, build_ybn_from_xml_checked};
pub use blocks::{Graph, BlockId, Built};
pub use math::{Vec2, Vec3, Vec4, Mat4};
pub use resource::{build_rsc7, build_rsc7_with_flags, pack_pages, rsc7_page_count, PagedLayout, is_fxap, FXAP_MAGIC, build_rsc7_paged, prepare_rsc7, rsc7_flags_for_pages, rsc7_flags_for_size, resource_size_from_flags, resource_version_from_flags, prepare_rsc7_system, inflate_rsc7_system,
                   RSC7_MAGIC, RSC8_MAGIC, SYSTEM_BASE, GRAPHICS_BASE};
pub use ytd::{parse_ytd, parse_ytd_system, serialize_ytd, full_mip_count, level_size, mip_chain_size, stride_for, to_dds_layout, to_ytd_layout, ytd_chain_size, TextureFormat, YtdTexture, YTD_VERSION};
pub use dds::parse_dds;
pub use gen9::Edition;
pub use blocks::texture::convert_ytd_to_legacy;
pub use schema::Schema;
pub use meta_write::{build_meta, Written, META_VERSION};
pub use pso_write::build_pso;
#[cfg(feature = "encode")]
pub use texture_encode::{auto_format, encode_texture, is_normal_map_name, EncodeFormat};
pub use ydd::{parse_ydd, parse_ydr, parse_drawables, Drawable, DrawableBounds, DrawableEntry,
              DrawableGeometry, DrawableKind, DrawableLod, DrawableModel, GeometryBounds,
              IndexBuffer, LodLevel, ShaderFx, ShaderGroup, ShaderParameter, ShaderParameterValue,
              UnifiedVertex, VertexAttribute, VertexAttributeValue, VertexBuffer,
              VertexBufferLayout, VertexComponent, VertexComponentType, VertexDeclaration,
              VertexSemantic, BUMP_SAMPLER, DIFFUSE_SAMPLER, SPEC_SAMPLER, TEXTURE_SAMPLER};
pub use yft::{parse_yft, wheel_slot, Fragment, FragmentChild, FragmentPart, WheelSlot};
pub use ymt::{parse_ymt, PedVariationInfo, PED_COMPONENT_NAMES};
pub use peds::{parse_peds_meta, MultiTxdRelationship, PedInitData, PedsMeta};
pub use vehicles::{parse_vehicles_meta, VehicleDriver, VehicleInitData, VehicleOverrideRagdollThreshold, VehiclesMeta};
pub use carcols::{parse_carcols, CarCols, VehicleKit, VehicleModelColor};
pub use carvariations::{parse_carvariations, CarVariations, ColorCombination, VehicleVariation};
pub use carmodcols::{parse_carmodcols, CarModCols, VehicleModColor};
pub use meta_read::{parse_tree, Fields, MetaEnum};
pub use ytyp::{parse_archetype_txds, parse_ytyp, Archetype, ArchetypeTxd, MloDef, MloEntitySet, MloPortal, MloRoom, Ytyp};
pub use gtxd::{parse_txd_relationships, TxdRelationship};
pub use ymap::{parse_ymap, parse_ymap_entities, parse_ymap_header, parse_ymap_mlo_instances, set_map_name, MloInstance, Ymap, YmapEntity, YmapHeader};
pub use ybn::{parse_ybn, Bound, BoundGeometry, BoundKind, BoundTransform, BoundTriangle, Triangle, Ybn};
pub use ynv::{parse_ynv, serialize_ynv, cell_bounds, cell_file_name, cell_for_position, NavEdge, NavEdgeEnd, NavPoint,
              NavPoly, NavPortal, Ynv, ADJACENT_NONE};
pub use ynd::{parse_ynd, serialize_ynd, ynd_to_xml, ynd_from_xml, dump_ynd_xml, build_ynd_from_xml, area_id_from_file_name as ynd_area_id_from_file_name,
              cell_bounds as ynd_cell_bounds, cell_file_name as ynd_cell_file_name, cell_for_position as ynd_cell_for_position,
              cell_of_area as ynd_cell_of_area, area_id as ynd_area_id, is_island_area as ynd_is_island_area, same_cell as ynd_same_cell,
              ISLAND_FLAG as YND_ISLAND_FLAG, Heightmap, NodeDictionary, NodeJunctionRef, NodeSpecial, NodeSpeed,
              PathJunction, PathLink, PathNode, Ynd};
pub use cache_dat::{parse_cache_dat, BoundsStoreItem, CacheDat, CacheFileDate, InteriorProxy, MapDataNode};
pub use heightmap::{parse_heightmap, serialize_heightmap, WorldHeightmap};
pub use water::{parse_water_xml, CalmingQuad, WaterData, WaterQuad, WaveQuad};
pub use value::{MetaArray, MetaDump, MetaStruct, MetaValue};
pub use pso::{dump_pso, is_pso, parse_pso, PsoFile, PSO_MAGIC};
pub use meta_schema::{dump_meta, parse_meta, HashSite, MetaFile};
pub use names::NameTable;
pub use xml::{from_xml, to_xml};
pub use json::{from_json, to_json};
pub use ymf::{dump_metadata, dump_ymf, parse_ymf, Dependencies, HashName, HdTxdBinding, ImapDependency, InteriorBounds, Manifest, ManifestFormat, MapDataGroup, MetaContainer};
pub use texture_utils::decompress_texture;
#[cfg(feature = "image")]
pub use texture_utils::{to_rgba_image, fit_max_size, encode_image, ImageFormat};
#[cfg(feature = "image")]
pub use image;
