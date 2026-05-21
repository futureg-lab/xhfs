use crate::xhfs::ds::*;
use bitvec::{bitvec, order::Msb0};

fn create_test_header(block_size: u64, groups: u64) -> XHFSHeader {
    XHFSHeader {
        version: 1,
        chacha20_nonce: Default::default(),
        extra_metadata: [0; 32],
        format: Format {
            param_data_block_count_per_group: 20480,
            param_inode_count_per_group: 4096,
            block_size_bytes: block_size,
            group_count: groups,
        },
    }
}

macro_rules! assert_contiguous {
    ($left:expr, $right:expr) => {
        assert_eq!(
            $left.end.get() + 1,
            $right.start.get(),
            "Regions are not contiguous! Left End: {}, Right Start: {}",
            $left.end.get(),
            $right.start.get()
        );
    };
}

fn setup_mock_geometry() -> GeometryLayout {
    let block_size = 4096;
    let blocks_per_group = 65536; // 256 MB per group
    let total_group_bytes = blocks_per_group * block_size; // 268 435 456 bytes
    let n_inodes_in_group = 8 * block_size; // 32 768 inodes per group

    GeometryLayout {
        rel_header_region: RegionSlot {
            start: 0u64.into(),
            end: 511u64.into(),
        },
        rel_data_bitmap_region: RegionSlot {
            start: 512u64.into(),
            end: 8703u64.into(),
        },
        rel_inode_bitmap_region: RegionSlot {
            start: 8704u64.into(),
            end: 12799u64.into(),
        },
        rel_inode_table_region: RegionSlot {
            start: 12800u64.into(),
            end: 8392703u64.into(),
        },
        rel_data_region: RegionSlot {
            start: 8392704u64.into(),
            end: (total_group_bytes - 1).into(),
        },
        n_inodes_in_group: n_inodes_in_group as u64,
        group_stride: total_group_bytes,
        usable_blocks_per_group: 63488,
    }
}

//////////////////////

#[test]
fn test_geometry_mathematical_invariants() -> eyre::Result<()> {
    // standard 256MB group with 4KB blocks
    let block_size = 4096;
    let blocks_per_group = 65536; // 256 MB / 4 KB
    let header = create_test_header(block_size, blocks_per_group);
    let (geometry, templates) = header.calculate_relative_geometry()?;

    assert_eq!(
        geometry.rel_header_region.start.get(),
        0,
        "Header must start at 0"
    );
    assert_contiguous!(geometry.rel_header_region, geometry.rel_data_bitmap_region);
    assert_contiguous!(
        geometry.rel_data_bitmap_region,
        geometry.rel_inode_bitmap_region
    );
    assert_contiguous!(
        geometry.rel_inode_bitmap_region,
        geometry.rel_inode_table_region
    );
    assert_contiguous!(geometry.rel_inode_table_region, geometry.rel_data_region);

    // Note: BAD
    // let expected_total_group_bytes = blocks_per_group * block_size;
    // assert_eq!(
    //     geometry.rel_data_region.end.get(),
    //     expected_total_group_bytes - 1,
    // );

    // IMPORTANT:
    // payload region size is a strict multiple of the block size
    let data_region_size_bytes =
        geometry.rel_data_region.end.get() - geometry.rel_data_region.start.get() + 1;
    assert_eq!(
        data_region_size_bytes % block_size,
        0,
        "data payload region size ({data_region_size_bytes}) must be a clean multiple of block size ({block_size})",
    );

    // check physical capacity sizes fi they correspond exactly to the serialized payload allocations
    assert_eq!(
        (geometry.rel_header_region.end.get() - geometry.rel_header_region.start.get() + 1),
        templates.serialized_header.len() as u64
    );
    assert_eq!(
        (geometry.rel_data_bitmap_region.end.get() - geometry.rel_data_bitmap_region.start.get()
            + 1),
        templates.data_block_bitmap.len() as u64
    );
    assert_eq!(
        (geometry.rel_inode_bitmap_region.end.get() - geometry.rel_inode_bitmap_region.start.get()
            + 1),
        templates.inode_bitmap_placeholder.len() as u64
    );
    assert_eq!(
        (geometry.rel_inode_table_region.end.get() - geometry.rel_inode_table_region.start.get()
            + 1),
        templates.inode_table_placeholder.len() as u64
    );

    Ok(())
}

#[test]
fn test_usable_blocks_derivation() -> eyre::Result<()> {
    let block_size = 4096;
    let blocks_per_group = 32768; // 128 MB Group
    let header = create_test_header(block_size, blocks_per_group);
    let (geometry, _) = header.calculate_relative_geometry()?;

    let data_region_bytes =
        geometry.rel_data_region.end.get() - geometry.rel_data_region.start.get() + 1;
    let derived_blocks = data_region_bytes / block_size;

    assert_eq!(geometry.usable_blocks_per_group, derived_blocks);
    assert!(
        geometry.usable_blocks_per_group < blocks_per_group,
        "usable block allocation to be lower than total group blocks to make sense of filesystem metadata tables"
    );

    Ok(())
}

#[test]
fn test_micro_embedded_disk_limits() -> eyre::Result<()> {
    // These are all inspired from real ext3, ext4 configurations
    let block_size = 512; // legacy small block layout
    let blocks_per_group = 2048; // 1 MB total size profile
    let header = create_test_header(block_size, blocks_per_group);
    let (geometry, templates) = header.calculate_relative_geometry()?;

    assert_contiguous!(geometry.rel_header_region, geometry.rel_data_bitmap_region);
    assert_contiguous!(
        geometry.rel_data_bitmap_region,
        geometry.rel_inode_bitmap_region
    );
    assert_contiguous!(
        geometry.rel_inode_bitmap_region,
        geometry.rel_inode_table_region
    );
    assert_contiguous!(geometry.rel_inode_table_region, geometry.rel_data_region);

    // check inode tables scaled based on the block size constraint (8 * 512 = 4096 INodes)
    assert_eq!(geometry.n_inodes_in_group, 4096);
    let expected_table_size = 4096 * INode::serialized_size() as u64;
    assert_eq!(
        templates.inode_table_placeholder.len() as u64,
        expected_table_size
    );

    assert!(
        geometry.usable_blocks_per_group > 0,
        "metadata completely exhausted the block group allocation, zero data blocks left for files"
    );

    Ok(())
}

#[test]
fn test_derive_address_group_zero_boundaries() {
    let geom = setup_mock_geometry();
    let layout = GroupLayout::derive_from_address(0, &geom).unwrap();
    assert_eq!(layout.g_index, 0);
    assert_eq!(layout.g_offset, 0);
    assert_eq!(layout.header_region.start.get(), 0);

    let last_byte_g0 = geom.group_stride - 1;
    let layout_last = GroupLayout::derive_from_address(last_byte_g0, &geom).unwrap();
    assert_eq!(layout_last.g_index, 0);
    assert_eq!(layout_last.g_offset, 0);
}

#[test]
fn test_derive_address_group_one_inflection_point() {
    let geom = setup_mock_geometry();
    let first_byte_g1 = geom.group_stride;
    let layout = GroupLayout::derive_from_address(first_byte_g1, &geom).unwrap();
    assert_eq!(layout.g_index, 1);
    assert_eq!(layout.g_offset, geom.group_stride);

    assert_eq!(layout.header_region.start.get(), geom.group_stride);
    assert_eq!(layout.data_region.end.get(), (geom.group_stride * 2) - 1);
}

#[test]
fn test_derive_address_deep_disk_translation() {
    let geom = setup_mock_geometry();
    let target_group = 52;
    let random_internal_offset = 1234567;
    let target_address = (target_group * geom.group_stride) + random_internal_offset;
    let layout = GroupLayout::derive_from_address(target_address, &geom).unwrap();

    assert_eq!(layout.g_index, target_group);
    assert_eq!(layout.g_offset, target_group * geom.group_stride);
    assert_eq!(
        layout.header_region.start.get(),
        target_group * geom.group_stride,
        "layout coordinates accurately added the massive stride offset"
    );
}

#[test]
fn test_derive_address_zero_stride_guard() {
    let mut geom = setup_mock_geometry();
    geom.group_stride = 0; // emulate uninitialized or malicious state
    let layout = GroupLayout::derive_from_address(1024, &geom);
    assert!(
        layout.is_none(),
        "should safely return None instead of panicking on division by zero"
    );
}

#[test]
fn test_derive_inode_zero_sentinel() {
    let geom = setup_mock_geometry();
    let layout = GroupLayout::derive_from_inode(0, &geom);
    assert!(
        layout.is_none(),
        "inumber 0 must return None as a special null case"
    );
}

#[test]
fn test_derive_inode_group_zero_boundaries() {
    let geom = setup_mock_geometry();

    let layout_first = GroupLayout::derive_from_inode(1, &geom).unwrap();
    assert_eq!(
        layout_first.g_index, 0,
        "test 1: 1 is the first valid inode, must map to group 0"
    );
    assert_eq!(
        layout_first.g_offset, 0,
        "test 2: 1 is the first valid inode, must map to group 0"
    );

    let last_inode_g0 = geom.n_inodes_in_group;
    let layout_last = GroupLayout::derive_from_inode(last_inode_g0, &geom).unwrap();
    assert_eq!(
        layout_last.g_index, 0,
        "absolute final inode tracked by group 0"
    );
    assert_eq!(
        layout_last.g_offset, 0,
        "absolute final inode tracked by group 0"
    );
}

#[test]
fn test_derive_inode_deep_index_generic() {
    let geom = setup_mock_geometry();
    let target_group = 42;
    let target_inode = (target_group * geom.n_inodes_in_group) + 372;
    let layout = GroupLayout::derive_from_inode(target_inode, &geom).unwrap();
    assert_eq!(layout.g_index, target_group);
    assert_eq!(layout.g_offset, target_group * geom.group_stride);
}

#[test]
fn test_derive_inode_group_one_inflection_point() {
    let geom = setup_mock_geometry();
    let first_inode_g1 = geom.n_inodes_in_group + 1;
    let layout = GroupLayout::derive_from_inode(first_inode_g1, &geom).unwrap();

    assert_eq!(layout.g_index, 1);
    assert_eq!(layout.g_offset, geom.group_stride);
    assert_eq!(
        layout.header_region.start.get(),
        geom.group_stride,
        "exec regional mapping applied the offset jump safely"
    );
}

#[test]
fn test_runs_of_zeros_empty_and_full() {
    // 0s
    let mut bitmap = Bitmap::new_from_bits_count(16);
    {
        let free_runs = bitmap.runs_of(false, None);
        assert_eq!(
            free_runs,
            [BitRunSlot { start: 0, size: 16 }],
            "find one massive 16 block free run"
        );

        let allocated_runs = bitmap.runs_of(true, None);
        assert!(allocated_runs.is_empty(), "find zero allocated runs");
    }
    // 1s
    {
        bitmap.map.fill(true);
        let free_runs_full = bitmap.runs_of(false, None);
        assert!(
            free_runs_full.is_empty(),
            "find zero free runs when disk is full"
        );
        let allocated_runs_full = bitmap.runs_of(true, None);
        assert_eq!(
            allocated_runs_full,
            [BitRunSlot { start: 0, size: 16 }],
            "Should find one massive 16-block continuous file"
        );
    }
}

#[test]
fn test_runs_of_fragmented_boundaries() -> eyre::Result<()> {
    let mut bitmap = Bitmap::new_from_bits_count(10);
    bitmap.set(4, true)?;
    bitmap.set(5, true)?;
    bitmap.set(6, true)?;

    let free_runs = bitmap.runs_of(false, None);
    assert_eq!(
        free_runs,
        [
            BitRunSlot { start: 0, size: 4 },
            BitRunSlot { start: 7, size: 3 }
        ],
        "isolate the two distinct gaps of free blocks"
    );

    let allocated_runs = bitmap.runs_of(true, None);
    assert_eq!(
        allocated_runs,
        [BitRunSlot { start: 4, size: 3 }],
        "find the exact single contiguous file fragment inside the group"
    );

    Ok(())
}

#[test]
fn test_runs_spanning_across_bytes() -> eyre::Result<()> {
    let mut bitmap = Bitmap {
        map: bitvec![u8, Msb0; 0; 16],
    };
    for i in 0..4 {
        bitmap.set(i, true)?;
    }
    for i in 12..16 {
        bitmap.set(i, true)?;
    }

    let free_runs = bitmap.runs_of(false, None);
    assert_eq!(
        free_runs,
        [BitRunSlot { start: 4, size: 8 }],
        "merge zero chunks across raw slice byte boundaries"
    );

    Ok(())
}

#[test]
fn test_runs_of_with_stop_index() {
    let bitmap = Bitmap::new_from_bits_count(24);

    let full_run = bitmap.runs_of(false, None);
    assert_eq!(
        full_run,
        [BitRunSlot { start: 0, size: 24 }],
        "scan the full capacity when no stop_index is set"
    );

    let truncated_run = bitmap.runs_of(false, Some(10));
    assert_eq!(
        truncated_run,
        [BitRunSlot { start: 0, size: 10 }],
        "cut off exactly at bit index 10, completely ignoring trailing bits"
    );
}
