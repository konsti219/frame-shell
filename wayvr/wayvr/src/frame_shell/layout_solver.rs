//! Pure 3D-to-2D layout solver mapping VR virtual panel poses onto gap-free, non-overlapping
//! 2D integer-logical-pixel compositor coordinates anchored to immutable physical monitors.

use crate::windowing::window::scalar_scale;
use glam::{Affine3A, Vec3A};
use std::collections::{BTreeMap, BTreeSet};
use wayvr_ipc::frame::PanelLayoutTarget;

#[derive(Debug, Clone)]
pub struct PhysicalAnchor {
    pub key: String,
    pub logical_pos: [i32; 2],
    pub logical_size: [i32; 2],
    pub overlay_transform: Option<Affine3A>,
}

#[derive(Debug, Clone)]
pub struct VirtualPanelPose {
    pub surface: u32,
    pub logical_size: [i32; 2],
    pub overlay_transform: Affine3A,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeSide {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedPlacement {
    pub parent_key: String,
    pub edge: EdgeSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone)]
struct PlacedRect {
    key: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl PlacedRect {
    fn intersects_strict(&self, x: i32, y: i32, w: i32, h: i32) -> bool {
        self.x < x + w && self.x + self.w > x && self.y < y + h && self.y + self.h > y
    }

    fn center(&self) -> (f32, f32) {
        (
            self.x as f32 + self.w as f32 * 0.5,
            self.y as f32 + self.h as f32 * 0.5,
        )
    }

    fn primary_start(&self, axis: Axis) -> i32 {
        match axis {
            Axis::Horizontal => self.x,
            Axis::Vertical => self.y,
        }
    }

    fn primary_len(&self, axis: Axis) -> i32 {
        match axis {
            Axis::Horizontal => self.w,
            Axis::Vertical => self.h,
        }
    }

    fn primary_end(&self, axis: Axis) -> i32 {
        self.primary_start(axis) + self.primary_len(axis)
    }

    fn cross_start(&self, axis: Axis) -> i32 {
        match axis {
            Axis::Horizontal => self.y,
            Axis::Vertical => self.x,
        }
    }

    fn cross_len(&self, axis: Axis) -> i32 {
        match axis {
            Axis::Horizontal => self.h,
            Axis::Vertical => self.w,
        }
    }

    fn shift(&mut self, axis: Axis, delta: i32) {
        match axis {
            Axis::Horizontal => self.x += delta,
            Axis::Vertical => self.y += delta,
        }
    }

    fn cross_overlap(&self, other: &Self, axis: Axis) -> (i32, i32) {
        let c0 = self.cross_start(axis).max(other.cross_start(axis));
        let c1 = (self.cross_start(axis) + self.cross_len(axis))
            .min(other.cross_start(axis) + other.cross_len(axis));
        (c0, c1)
    }

    fn shared_edge_len(&self, other: &Self) -> i32 {
        let x_ov = (self.x + self.w).min(other.x + other.w) - self.x.max(other.x);
        let y_ov = (self.y + self.h).min(other.y + other.h) - self.y.max(other.y);
        let touch_x = self.x + self.w == other.x || other.x + other.w == self.x;
        let touch_y = self.y + self.h == other.y || other.y + other.h == self.y;
        if touch_x && y_ov > 0 {
            y_ov
        } else if touch_y && x_ov > 0 {
            x_ov
        } else {
            0
        }
    }
}

fn min_edge_overlap(a: &PlacedRect, b: &PlacedRect) -> i32 {
    48.min(a.w).min(a.h).min(b.w).min(b.h).max(1)
}

fn is_valid_layout(rects: &[PlacedRect], phys_count: usize) -> bool {
    for i in 0..rects.len() {
        for j in (i + 1)..rects.len() {
            let b = &rects[j];
            if rects[i].intersects_strict(b.x, b.y, b.w, b.h) {
                return false;
            }
        }
    }
    if phys_count == 0 {
        return true;
    }
    let mut visited = vec![false; rects.len()];
    let mut stack = Vec::with_capacity(rects.len());
    for i in 0..phys_count {
        visited[i] = true;
        stack.push(i);
    }
    while let Some(cur) = stack.pop() {
        for nxt in 0..rects.len() {
            if visited[nxt] {
                continue;
            }
            if rects[cur].shared_edge_len(&rects[nxt]) >= min_edge_overlap(&rects[cur], &rects[nxt])
            {
                visited[nxt] = true;
                stack.push(nxt);
            }
        }
    }
    visited.iter().all(|&v| v)
}

fn rigid_cluster(
    rects: &[PlacedRect],
    phys_count: usize,
    start: usize,
    axis: Axis,
) -> BTreeSet<usize> {
    let mut cluster = BTreeSet::new();
    if start < phys_count {
        return cluster;
    }
    let mut stack = vec![start];
    cluster.insert(start);
    while let Some(cur) = stack.pop() {
        let a = &rects[cur];
        for nxt in phys_count..rects.len() {
            if cluster.contains(&nxt) {
                continue;
            }
            let b = &rects[nxt];
            let touch_primary =
                a.primary_end(axis) == b.primary_start(axis) || b.primary_end(axis) == a.primary_start(axis);
            let (c0, c1) = a.cross_overlap(b, axis);
            if touch_primary && c1 > c0 {
                cluster.insert(nxt);
                stack.push(nxt);
            }
        }
    }
    cluster
}

/// Deterministically closes small dead corridors between edge-facing rectangles along `axis`
/// while keeping physical monitors fixed and preserving overlap/connectivity invariants.
fn compact_axis(rects: &mut Vec<PlacedRect>, phys_count: usize, axis: Axis) {
    let max_steps = rects.len() * rects.len() + 4;
    let mut tried: BTreeSet<(usize, usize)> = BTreeSet::new();

    for _ in 0..max_steps {
        let mut best_pair: Option<(i32, usize, usize)> = None;
        for i in 0..rects.len() {
            for j in 0..rects.len() {
                if i == j || (i < phys_count && j < phys_count) || tried.contains(&(i, j)) {
                    continue;
                }
                let gap = rects[j].primary_start(axis) - rects[i].primary_end(axis);
                if gap <= 0 {
                    continue;
                }
                let (c0, c1) = rects[i].cross_overlap(&rects[j], axis);
                let cross_ov = c1 - c0;
                let min_cross = 48
                    .min(rects[i].cross_len(axis))
                    .min(rects[j].cross_len(axis))
                    .max(1);
                if cross_ov < min_cross {
                    continue;
                }
                let max_gap = if i >= phys_count && j >= phys_count {
                    rects[i].primary_len(axis) + rects[j].primary_len(axis)
                } else {
                    256
                };
                if gap > max_gap {
                    continue;
                }
                let gap_start = rects[i].primary_end(axis);
                let gap_end = rects[j].primary_start(axis);
                let obstructed = (0..rects.len()).any(|k| {
                    if k == i || k == j {
                        return false;
                    }
                    let r = &rects[k];
                    r.primary_start(axis) < gap_end
                        && r.primary_end(axis) > gap_start
                        && r.cross_start(axis) < c1
                        && r.cross_start(axis) + r.cross_len(axis) > c0
                });
                if obstructed {
                    continue;
                }
                let key = (gap, i, j);
                if best_pair.is_none_or(|b| key < b) {
                    best_pair = Some(key);
                }
            }
        }

        let Some((gap, i, j)) = best_pair else {
            break;
        };
        tried.insert((i, j));

        let left_cluster = rigid_cluster(rects, phys_count, i, axis);
        let right_cluster = rigid_cluster(rects, phys_count, j, axis);
        if left_cluster.is_empty() && right_cluster.is_empty() {
            continue;
        }
        if !left_cluster.is_disjoint(&right_cluster)
            || left_cluster.iter().any(|&a| {
                right_cluster
                    .iter()
                    .any(|&b| rects[a].shared_edge_len(&rects[b]) > 0)
            })
        {
            continue;
        }

        let mut obs_limit_l = if left_cluster.is_empty() { 0 } else { gap };
        for &u in &left_cluster {
            for (k, other) in rects.iter().enumerate() {
                if left_cluster.contains(&k) || right_cluster.contains(&k) {
                    continue;
                }
                let (c0, c1) = rects[u].cross_overlap(other, axis);
                if c1 > c0 && other.primary_start(axis) >= rects[u].primary_end(axis) {
                    obs_limit_l =
                        obs_limit_l.min(other.primary_start(axis) - rects[u].primary_end(axis));
                }
            }
        }

        let mut obs_limit_r = if right_cluster.is_empty() { 0 } else { gap };
        for &v in &right_cluster {
            for (k, other) in rects.iter().enumerate() {
                if left_cluster.contains(&k) || right_cluster.contains(&k) {
                    continue;
                }
                let (c0, c1) = rects[v].cross_overlap(other, axis);
                if c1 > c0 && rects[v].primary_start(axis) >= other.primary_end(axis) {
                    obs_limit_r =
                        obs_limit_r.min(rects[v].primary_start(axis) - other.primary_end(axis));
                }
            }
        }

        if obs_limit_l + obs_limit_r < gap {
            continue;
        }

        let (dl, dr) = if left_cluster.is_empty() {
            (0, gap)
        } else if right_cluster.is_empty() {
            (gap, 0)
        } else {
            let total = (left_cluster.len() + right_cluster.len()) as i32;
            let mut dl = (gap * right_cluster.len() as i32) / total;
            let mut dr = gap - dl;
            if dl > obs_limit_l {
                dl = obs_limit_l;
                dr = gap - dl;
            }
            if dr > obs_limit_r {
                dr = obs_limit_r;
                dl = gap - dr;
            }
            (dl, dr)
        };

        let mut candidates = vec![(dl, dr)];
        if !right_cluster.is_empty() && obs_limit_r >= gap && (dl, dr) != (0, gap) {
            candidates.push((0, gap));
        }
        if !left_cluster.is_empty() && obs_limit_l >= gap && (dl, dr) != (gap, 0) {
            candidates.push((gap, 0));
        }

        for (step_l, step_r) in candidates {
            let mut trial = rects.clone();
            for &u in &left_cluster {
                trial[u].shift(axis, step_l);
            }
            for &v in &right_cluster {
                trial[v].shift(axis, -step_r);
            }
            if is_valid_layout(&trial, phys_count) {
                *rects = trial;
                tried.clear();
                break;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct LayoutSolution {
    pub targets: Vec<PanelLayoutTarget>,
    pub committed: BTreeMap<u32, CommittedPlacement>,
}

fn deduplicate_physical(physical: &[PhysicalAnchor]) -> Vec<PhysicalAnchor> {
    let mut unique: Vec<PhysicalAnchor> = Vec::new();
    for anchor in physical {
        if anchor.logical_size[0] <= 0 || anchor.logical_size[1] <= 0 {
            continue;
        }
        if let Some(existing) = unique
            .iter_mut()
            .find(|u| u.logical_pos == anchor.logical_pos && u.logical_size == anchor.logical_size)
        {
            if existing.overlay_transform.is_none() && anchor.overlay_transform.is_some() {
                existing.overlay_transform = anchor.overlay_transform;
                existing.key = anchor.key.clone();
            }
            continue;
        }
        unique.push(anchor.clone());
    }
    unique
}

fn pick_primary_anchor(
    unique_physical: &[PhysicalAnchor],
    fallback_frame: Option<Affine3A>,
) -> (PhysicalAnchor, Affine3A) {
    let best = unique_physical
        .iter()
        .min_by_key(|a| {
            let has_pose = a.overlay_transform.is_none();
            let at_origin = a.logical_pos != [0, 0];
            let area = -(i64::from(a.logical_size[0]) * i64::from(a.logical_size[1]));
            (has_pose, at_origin, area, a.logical_pos[1], a.logical_pos[0], a.key.clone())
        })
        .cloned()
        .unwrap_or_else(|| PhysicalAnchor {
            key: "fallback".into(),
            logical_pos: [0, 0],
            logical_size: [1920, 1080],
            overlay_transform: fallback_frame,
        });

    let default_pose = Affine3A::from_scale_rotation_translation(
        glam::Vec3::splat(1.5),
        glam::Quat::IDENTITY,
        glam::vec3(0.0, 0.0, -1.0),
    );
    let basis = best
        .overlay_transform
        .or(fallback_frame)
        .unwrap_or(default_pose);
    (best, basis)
}

fn safe_unit(v: Vec3A, fallback: Vec3A) -> Vec3A {
    let len = v.length();
    if len > 1e-4 { v / len } else { fallback }
}

fn snap_axis(raw: i32, panel_len: i32, parent_start: i32, parent_len: i32, min_val: i32, max_val: i32) -> i32 {
    const SNAP_PX: i32 = 32;
    let candidates = [
        parent_start,
        parent_start + parent_len - panel_len,
        parent_start + (parent_len - panel_len) / 2,
    ];
    let snapped = candidates
        .into_iter()
        .find(|&anchor| (raw - anchor).abs() <= SNAP_PX)
        .unwrap_or(raw);
    snapped.clamp(min_val, max_val.max(min_val))
}

/// Computes non-overlapping, edge-snapped 2D logical coordinates for `virtuals` around immutable
/// `physical` monitor anchors.
pub fn solve_layout(
    physical: &[PhysicalAnchor],
    virtuals: &[VirtualPanelPose],
    fallback_frame: Option<Affine3A>,
    previous: &BTreeMap<u32, CommittedPlacement>,
) -> LayoutSolution {
    if virtuals.is_empty() {
        return LayoutSolution {
            targets: Vec::new(),
            committed: BTreeMap::new(),
        };
    }

    let unique_physical = deduplicate_physical(physical);
    let (primary, basis) = pick_primary_anchor(&unique_physical, fallback_frame);

    let mut placed: Vec<PlacedRect> = unique_physical
        .iter()
        .map(|p| PlacedRect {
            key: p.key.clone(),
            x: p.logical_pos[0],
            y: p.logical_pos[1],
            w: p.logical_size[0],
            h: p.logical_size[1],
        })
        .collect();
    if placed.is_empty() {
        placed.push(PlacedRect {
            key: primary.key.clone(),
            x: primary.logical_pos[0],
            y: primary.logical_pos[1],
            w: primary.logical_size[0],
            h: primary.logical_size[1],
        });
    }

    let phys_count = placed.len();

    // Desk basis anchored to primary physical overlay (head-rotation invariant).
    let right = safe_unit(basis.matrix3.x_axis, Vec3A::X);
    let up = safe_unit(basis.matrix3.y_axis, Vec3A::Y);
    let toward_user = safe_unit(basis.matrix3.z_axis, Vec3A::Z);
    let forward = -toward_user;
    let primary_scale = scalar_scale(&basis).abs().max(0.25);
    let px_per_m = primary.logical_size[0].max(primary.logical_size[1]).max(1) as f32 / primary_scale;
    let desk_radius = (primary_scale * 1.15).max(1.0);
    let pivot = basis.translation - forward * desk_radius;
    let primary_cx = primary.logical_pos[0] as f32 + primary.logical_size[0] as f32 * 0.5;
    let primary_cy = primary.logical_pos[1] as f32 + primary.logical_size[1] as f32 * 0.5;

    let mut ordered: Vec<&VirtualPanelPose> = virtuals.iter().collect();
    ordered.sort_by(|a, b| {
        let da = (a.overlay_transform.translation - basis.translation).length_squared();
        let db = (b.overlay_transform.translation - basis.translation).length_squared();
        da.total_cmp(&db).then_with(|| a.surface.cmp(&b.surface))
    });

    let mut virtual_indices: Vec<(u32, usize)> = Vec::with_capacity(ordered.len());
    let mut committed = BTreeMap::new();

    for panel in ordered {
        let vw = panel.logical_size[0].max(1);
        let vh = panel.logical_size[1].max(1);

        let u = panel.overlay_transform.translation - pivot;
        let angle = f32::atan2(u.dot(right), u.dot(forward).max(0.15));
        let dx_m = angle * desk_radius;
        let dy_m = (panel.overlay_transform.translation - basis.translation).dot(up);
        let target_cx = primary_cx + dx_m * px_per_m;
        let target_cy = primary_cy - dy_m * px_per_m;

        let mut best: Option<(f32, usize, EdgeSide, i32, i32, String)> = None;
        for (parent_idx, parent) in placed.iter().enumerate() {
            let min_ox = 48.min(vw).min(parent.w).max(1);
            let min_oy = 48.min(vh).min(parent.h).max(1);
            let raw_x = (target_cx - vw as f32 * 0.5).round() as i32;
            let raw_y = (target_cy - vh as f32 * 0.5).round() as i32;

            let x_snapped = snap_axis(
                raw_x,
                vw,
                parent.x,
                parent.w,
                parent.x - vw + min_ox,
                parent.x + parent.w - min_ox,
            );
            let y_snapped = snap_axis(
                raw_y,
                vh,
                parent.y,
                parent.h,
                parent.y - vh + min_oy,
                parent.y + parent.h - min_oy,
            );

            let candidates = [
                (EdgeSide::Top, x_snapped, parent.y - vh),
                (EdgeSide::Bottom, x_snapped, parent.y + parent.h),
                (EdgeSide::Left, parent.x - vw, y_snapped),
                (EdgeSide::Right, parent.x + parent.w, y_snapped),
            ];

            for (edge, x, y) in candidates {
                if placed.iter().any(|r| r.intersects_strict(x, y, vw, vh)) {
                    continue;
                }
                let cx = x as f32 + vw as f32 * 0.5;
                let cy = y as f32 + vh as f32 * 0.5;
                let dist = (cx - target_cx).hypot(cy - target_cy);
                let sticky = previous.get(&panel.surface).is_some_and(|prev| {
                    prev.parent_key == parent.key && prev.edge == edge
                });
                let score = if sticky { dist - 64.0 } else { dist };
                let better = best.as_ref().is_none_or(|b| {
                    score
                        .total_cmp(&b.0)
                        .then_with(|| parent_idx.cmp(&b.1))
                        .then_with(|| edge.cmp(&b.2))
                        .then_with(|| x.cmp(&b.3))
                        .then_with(|| y.cmp(&b.4))
                        .is_lt()
                });
                if better {
                    best = Some((score, parent_idx, edge, x, y, parent.key.clone()));
                }
            }
        }

        let (x, y, commit) = match best {
            Some((_, _, edge, x, y, parent_key)) => {
                (x, y, CommittedPlacement { parent_key, edge })
            }
            None => {
                // Guaranteed non-overlapping rightmost fallback attached to a rightmost placed rect.
                let max_x = placed.iter().map(|r| r.x + r.w).max().unwrap_or(0);
                let anchor = placed
                    .iter()
                    .filter(|r| r.x + r.w == max_x)
                    .min_by(|a, b| {
                        let da = (a.center().1 - target_cy).abs();
                        let db = (b.center().1 - target_cy).abs();
                        da.total_cmp(&db).then_with(|| a.y.cmp(&b.y))
                    })
                    .unwrap_or(&placed[0]);
                (
                    max_x,
                    anchor.y,
                    CommittedPlacement {
                        parent_key: anchor.key.clone(),
                        edge: EdgeSide::Right,
                    },
                )
            }
        };

        let placed_idx = placed.len();
        placed.push(PlacedRect {
            key: format!("v:{}", panel.surface),
            x,
            y,
            w: vw,
            h: vh,
        });
        committed.insert(panel.surface, commit);
        virtual_indices.push((panel.surface, placed_idx));
    }

    compact_axis(&mut placed, phys_count, Axis::Horizontal);
    compact_axis(&mut placed, phys_count, Axis::Vertical);

    let mut targets: Vec<PanelLayoutTarget> = virtual_indices
        .into_iter()
        .map(|(surface, idx)| PanelLayoutTarget {
            surface,
            x: placed[idx].x,
            y: placed[idx].y,
        })
        .collect();
    targets.sort_by_key(|t| t.surface);
    LayoutSolution { targets, committed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Quat, vec3};

    fn pose(x: f32, y: f32, z: f32, scale: f32) -> Affine3A {
        Affine3A::from_scale_rotation_translation(glam::Vec3::splat(scale), Quat::IDENTITY, vec3(x, y, z))
    }

    fn phys(key: &str, x: i32, y: i32, w: i32, h: i32, transform: Option<Affine3A>) -> PhysicalAnchor {
        PhysicalAnchor {
            key: key.into(),
            logical_pos: [x, y],
            logical_size: [w, h],
            overlay_transform: transform,
        }
    }

    fn virt(surface: u32, w: i32, h: i32, transform: Affine3A) -> VirtualPanelPose {
        VirtualPanelPose {
            surface,
            logical_size: [w, h],
            overlay_transform: transform,
        }
    }

    fn shared_edge_len(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> i32 {
        let ra = PlacedRect {
            key: String::new(),
            x: a.0,
            y: a.1,
            w: a.2,
            h: a.3,
        };
        let rb = PlacedRect {
            key: String::new(),
            x: b.0,
            y: b.1,
            w: b.2,
            h: b.3,
        };
        ra.shared_edge_len(&rb)
    }

    fn assert_invariants(
        physical: &[PhysicalAnchor],
        virtuals: &[VirtualPanelPose],
        solution: &LayoutSolution,
    ) {
        let unique = deduplicate_physical(physical);
        let mut rects: Vec<(i32, i32, i32, i32)> = unique
            .iter()
            .map(|p| (p.logical_pos[0], p.logical_pos[1], p.logical_size[0], p.logical_size[1]))
            .collect();
        let phys_count = rects.len();
        for t in &solution.targets {
            let v = virtuals.iter().find(|v| v.surface == t.surface).unwrap();
            rects.push((t.x, t.y, v.logical_size[0], v.logical_size[1]));
        }
        // Zero strict interior overlap between any two monitors
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let a = rects[i];
                let b = rects[j];
                let overlap = a.0 < b.0 + b.2 && a.0 + a.2 > b.0 && a.1 < b.1 + b.3 && a.1 + a.3 > b.1;
                assert!(!overlap, "Rectangles {i}={a:?} and {j}={b:?} overlap");
            }
        }
        // Every virtual monitor is connected via >= min_overlap edge contact to physical monitors
        let mut visited = vec![false; rects.len()];
        let mut stack = Vec::new();
        for i in 0..phys_count {
            visited[i] = true;
            stack.push(i);
        }
        while let Some(cur) = stack.pop() {
            for nxt in 0..rects.len() {
                if visited[nxt] {
                    continue;
                }
                let a = rects[cur];
                let b = rects[nxt];
                let min_edge = 48.min(a.2).min(a.3).min(b.2).min(b.3).max(1);
                if shared_edge_len(a, b) >= min_edge {
                    visited[nxt] = true;
                    stack.push(nxt);
                }
            }
        }
        assert!(visited.iter().all(|&v| v), "Disconnected virtual monitor in {rects:?}");
    }

    #[test]
    fn single_monitor_cardinal_and_diagonal_directions() {
        let physical = vec![phys("DP-1", 0, 0, 2560, 1440, Some(pose(0.0, 0.0, -1.5, 1.5)))];

        // Above
        let v_above = vec![virt(0x8000_0000, 1920, 1080, pose(0.0, 1.0, -1.5, 1.2))];
        let sol = solve_layout(&physical, &v_above, None, &BTreeMap::new());
        assert_eq!(sol.targets[0].y, -1080);
        assert_invariants(&physical, &v_above, &sol);

        // Below
        let v_below = vec![virt(0x8000_0000, 1920, 1080, pose(0.0, -1.0, -1.5, 1.2))];
        let sol = solve_layout(&physical, &v_below, None, &BTreeMap::new());
        assert_eq!(sol.targets[0].y, 1440);
        assert_invariants(&physical, &v_below, &sol);

        // Left
        let v_left = vec![virt(0x8000_0000, 1920, 1080, pose(-1.4, 0.0, -1.5, 1.2))];
        let sol = solve_layout(&physical, &v_left, None, &BTreeMap::new());
        assert_eq!(sol.targets[0].x, -1920);
        assert_invariants(&physical, &v_left, &sol);

        // Right
        let v_right = vec![virt(0x8000_0000, 1920, 1080, pose(1.4, 0.0, -1.5, 1.2))];
        let sol = solve_layout(&physical, &v_right, None, &BTreeMap::new());
        assert_eq!(sol.targets[0].x, 2560);
        assert_invariants(&physical, &v_right, &sol);

        // Above-right
        let v_diag = vec![virt(0x8000_0000, 1280, 720, pose(1.3, 0.9, -1.5, 1.0))];
        let sol = solve_layout(&physical, &v_diag, None, &BTreeMap::new());
        assert_invariants(&physical, &v_diag, &sol);
    }

    #[test]
    fn dual_physical_and_three_curved_arc_panels() {
        let physical = vec![
            phys("HDMI-A-2", -1920, 0, 1920, 1080, Some(pose(-1.3, 0.0, -1.4, 1.2))),
            phys("DP-4", 0, 0, 2560, 1080, Some(pose(0.0, 0.0, -1.5, 1.5))),
        ];
        let virtuals = vec![
            virt(0x8000_0000, 1920, 1080, pose(-0.9, 0.9, -1.4, 1.1)),
            virt(0x8000_0001, 1920, 1080, pose(0.3, 0.9, -1.5, 1.1)),
            virt(0x8000_0002, 1280, 1440, pose(1.6, 0.1, -1.1, 1.0)),
        ];
        let sol = solve_layout(&physical, &virtuals, None, &BTreeMap::new());
        assert_eq!(sol.targets.len(), 3);
        assert_invariants(&physical, &virtuals, &sol);
    }

    #[test]
    fn mirrored_physical_monitors_and_fractional_scale() {
        let physical = vec![
            phys("DP-1", 0, 0, 2560, 1440, Some(pose(0.0, 0.0, -1.5, 1.5))),
            phys("HDMI-A-1", 0, 0, 2560, 1440, None),
        ];
        let virtuals = vec![virt(0x8000_0000, 1280, 1440, pose(0.0, 1.1, -1.5, 1.0))];
        let sol = solve_layout(&physical, &virtuals, None, &BTreeMap::new());
        assert_eq!(sol.targets[0].y, -1440);
        assert_invariants(&physical, &virtuals, &sol);
    }

    #[test]
    fn hysteresis_stabilizes_near_diagonal() {
        let physical = vec![phys("DP-1", 0, 0, 1920, 1080, Some(pose(0.0, 0.0, -1.5, 1.5)))];
        let v_above = vec![virt(0x8000_0000, 1920, 1080, pose(1.2, 0.85, -1.5, 1.2))];
        let sol1 = solve_layout(&physical, &v_above, None, &BTreeMap::new());
        assert_eq!(sol1.committed[&0x8000_0000].edge, EdgeSide::Top);

        // Slight nudge toward diagonal shouldn't flip immediately due to 64px hysteresis.
        let v_nudge = vec![virt(0x8000_0000, 1920, 1080, pose(1.25, 0.78, -1.5, 1.2))];
        let sol2 = solve_layout(&physical, &v_nudge, None, &sol1.committed);
        assert_eq!(sol2.committed[&0x8000_0000].edge, EdgeSide::Top);
        assert_invariants(&physical, &v_nudge, &sol2);
    }

    #[test]
    fn crowded_stack_uses_guaranteed_non_overlapping_fallback() {
        let physical = vec![
            phys("C", 0, 0, 1920, 1080, Some(pose(0.0, 0.0, -1.5, 1.5))),
            phys("T", 0, -1080, 1920, 1080, None),
            phys("B", 0, 1080, 1920, 1080, None),
            phys("L", -1920, 0, 1920, 1080, None),
            phys("R", 1920, 0, 1920, 1080, None),
        ];
        // Virtual placed close to center where direct edges on C are blocked on all 4 sides
        let virtuals = vec![virt(0x8000_0000, 1920, 1080, pose(0.0, 0.0, -1.2, 1.2))];
        let sol = solve_layout(&physical, &virtuals, None, &BTreeMap::new());
        assert_invariants(&physical, &virtuals, &sol);
    }

    #[test]
    fn two_virtuals_above_ultrawide_close_horizontal_gap() {
        let physical = vec![
            phys("HDMI-A-2", -1920, 0, 1920, 1080, None),
            phys("DP-4", 0, 0, 2560, 1080, Some(pose(0.0, 0.0, -1.5, 1.5))),
        ];
        // Spaced wide enough above DP-4 that raw greedy attachment leaves a ~481px corridor.
        let virtuals = vec![
            virt(0x8000_0000, 1920, 1080, pose(0.915, 0.65, -1.5, 1.1)),
            virt(0x8000_0001, 1920, 1080, pose(-0.585, 0.65, -1.5, 1.1)),
        ];
        let sol = solve_layout(&physical, &virtuals, None, &BTreeMap::new());
        assert_invariants(&physical, &virtuals, &sol);
        assert_eq!(sol.targets[0].y, -1080);
        assert_eq!(sol.targets[1].y, -1080);
        let a = (sol.targets[0].x, sol.targets[0].y, 1920, 1080);
        let b = (sol.targets[1].x, sol.targets[1].y, 1920, 1080);
        assert_eq!(shared_edge_len(a, b), 1080, "expected flush seam between {a:?} and {b:?}");
    }

    #[test]
    fn three_virtuals_in_top_row_pack_without_gaps() {
        let physical = vec![
            phys("HDMI-A-2", -1920, 0, 1920, 1080, None),
            phys("DP-4", 0, 0, 2560, 1080, Some(pose(0.0, 0.0, -1.5, 1.5))),
        ];
        let virtuals = vec![
            virt(0x8000_0000, 1920, 1080, pose(-1.55, 0.65, -1.5, 1.1)),
            virt(0x8000_0001, 1920, 1080, pose(0.0, 0.65, -1.5, 1.1)),
            virt(0x8000_0002, 1920, 1080, pose(1.45, 0.65, -1.5, 1.1)),
        ];
        let sol = solve_layout(&physical, &virtuals, None, &BTreeMap::new());
        assert_invariants(&physical, &virtuals, &sol);
        let mut rects: Vec<_> = sol
            .targets
            .iter()
            .map(|t| (t.x, t.y, 1920, 1080))
            .collect();
        rects.sort_by_key(|r| r.0);
        assert_eq!(shared_edge_len(rects[0], rects[1]), 1080, "rects={rects:?}");
        assert_eq!(shared_edge_len(rects[1], rects[2]), 1080, "rects={rects:?}");
    }

    #[test]
    fn two_virtuals_stacked_vertically_close_vertical_gap() {
        let physical = vec![phys("DP-4", 0, 0, 1440, 2560, Some(pose(0.0, 0.0, -1.5, 1.5)))];
        let virtuals = vec![
            virt(0x8000_0000, 1280, 720, pose(1.1, 0.45, -1.5, 1.0)),
            virt(0x8000_0001, 1280, 720, pose(1.1, -0.45, -1.5, 1.0)),
        ];
        let sol = solve_layout(&physical, &virtuals, None, &BTreeMap::new());
        assert_invariants(&physical, &virtuals, &sol);
        let a = (sol.targets[0].x, sol.targets[0].y, 1280, 720);
        let b = (sol.targets[1].x, sol.targets[1].y, 1280, 720);
        assert_eq!(shared_edge_len(a, b), 1280, "expected vertical stack flush seam between {a:?} and {b:?}");
    }

    #[test]
    fn physical_monitor_between_virtuals_is_not_collapsed() {
        let physical = vec![phys("DP-4", 0, 0, 2560, 1080, Some(pose(0.0, 0.0, -1.5, 1.5)))];
        let virtuals = vec![
            virt(0x8000_0000, 1920, 1080, pose(-1.4, 0.0, -1.5, 1.2)),
            virt(0x8000_0001, 1920, 1080, pose(1.4, 0.0, -1.5, 1.2)),
        ];
        let sol = solve_layout(&physical, &virtuals, None, &BTreeMap::new());
        assert_invariants(&physical, &virtuals, &sol);
        assert_eq!(sol.targets[0].x, -1920);
        assert_eq!(sol.targets[1].x, 2560);
    }

    #[test]
    fn different_height_side_by_side_virtuals_touch() {
        let physical = vec![
            phys("HDMI-A-2", -1920, 0, 1920, 1080, None),
            phys("DP-4", 0, 0, 2560, 1080, Some(pose(0.0, 0.0, -1.5, 1.5))),
        ];
        let virtuals = vec![
            virt(0x8000_0000, 1280, 1440, pose(-0.585, 0.65, -1.5, 1.0)),
            virt(0x8000_0001, 1920, 1080, pose(0.915, 0.65, -1.5, 1.1)),
        ];
        let sol = solve_layout(&physical, &virtuals, None, &BTreeMap::new());
        assert_invariants(&physical, &virtuals, &sol);
        let a = (sol.targets[0].x, sol.targets[0].y, 1280, 1440);
        let b = (sol.targets[1].x, sol.targets[1].y, 1920, 1080);
        assert_eq!(a.1 + a.3, 0);
        assert_eq!(b.1 + b.3, 0);
        assert_eq!(shared_edge_len(a, b), 1080, "expected flush seam between {a:?} and {b:?}");
    }

    #[test]
    fn compaction_is_idempotent() {
        let physical = vec![
            phys("HDMI-A-2", -1920, 0, 1920, 1080, None),
            phys("DP-4", 0, 0, 2560, 1080, Some(pose(0.0, 0.0, -1.5, 1.5))),
        ];
        let virtuals = vec![
            virt(0x8000_0000, 1920, 1080, pose(0.915, 0.65, -1.5, 1.1)),
            virt(0x8000_0001, 1920, 1080, pose(-0.585, 0.65, -1.5, 1.1)),
        ];
        let sol1 = solve_layout(&physical, &virtuals, None, &BTreeMap::new());
        let sol2 = solve_layout(&physical, &virtuals, None, &sol1.committed);
        assert_eq!(sol1.targets, sol2.targets);
    }
}
