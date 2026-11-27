//! Pure 3D-to-2D layout solver mapping VR virtual panel poses onto gap-free, non-overlapping
//! 2D integer-logical-pixel compositor coordinates anchored to immutable physical monitors.

use crate::windowing::window::scalar_scale;
use glam::{Affine3A, Vec3A};
use std::collections::BTreeMap;
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

    // Desk basis anchored to primary physical overlay (head-rotation invariant).
    let right = safe_unit(basis.matrix3.x_axis, Vec3A::X);
    let up = safe_unit(basis.matrix3.y_axis, Vec3A::Y);
    let toward_user = safe_unit(basis.matrix3.z_axis, Vec3A::Z);
    let forward = -toward_user;
    let primary_scale = scalar_scale(&basis).abs().max(0.25);
    let px_per_m = primary.logical_size[0].max(1) as f32 / primary_scale;
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

    let mut targets = Vec::with_capacity(ordered.len());
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

        placed.push(PlacedRect {
            key: format!("v:{}", panel.surface),
            x,
            y,
            w: vw,
            h: vh,
        });
        committed.insert(panel.surface, commit);
        targets.push(PanelLayoutTarget {
            surface: panel.surface,
            x,
            y,
        });
    }

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
        let x_ov = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
        let y_ov = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
        let touch_x = a.0 + a.2 == b.0 || b.0 + b.2 == a.0;
        let touch_y = a.1 + a.3 == b.1 || b.1 + b.3 == a.1;
        if touch_x && y_ov > 0 {
            y_ov
        } else if touch_y && x_ov > 0 {
            x_ov
        } else {
            0
        }
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
}
