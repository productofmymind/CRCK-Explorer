//! The transform gizmo on the selection: arrows and plane handles to move, rings to rotate, handles to scale; in world or local space, on one
//! entity or on a group about its middle; with grid, ground and angle snapping; Shift-drag copies. The maths is kept apart from the drawing
//! so it can be tested.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Space {
    World,
    Local,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SnapMode {
    None,
    /// positions on a grid, angles in steps
    Grid,
    /// z follows the ground under the entity
    Ground,
    /// the grid for x and y, then the ground
    Hybrid,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Handle {
    /// move along an axis / rotate about it / scale along it
    Axis(usize),
    /// move in the plane whose normal is this axis
    Plane(usize),
    /// scale every way at once
    Uniform,
}

/// a drag in progress
pub(super) struct Drag {
    tool: Tool,
    handle: Handle,
    starts: Vec<(u32, EntityEdit)>,
    pivot: Vec3,
    frame: Quat,
    /// along the axis (move, scale) or the angle (rotate) the mouse started at
    t0: f32,
    /// where the mouse ray first met the plane (plane moves)
    hit0: Vec3,
    len: f32,
    y0: f32,
}

pub(super) const COLORS: [Color32; 3] = [Color32::from_rgb(235, 70, 70), Color32::from_rgb(80, 215, 90), Color32::from_rgb(80, 130, 245)];

/// the two in-plane axes of the plane whose normal is axis `n` (right-handed: u x v = the normal)
pub(super) fn plane_basis(n: usize, axes: [Vec3; 3]) -> (Vec3, Vec3) {
    match n {
        0 => (axes[1], axes[2]),
        1 => (axes[2], axes[0]),
        _ => (axes[0], axes[1]),
    }
}

/// the point where the mouse ray meets the plane through `point` with unit normal `normal`
pub(super) fn ray_plane(vp: &Mat4, rect: egui::Rect, eye: Vec3, cursor: Pos2, point: Vec3, normal: Vec3) -> Option<Vec3> {
    let r = ray_dir(vp, rect, eye, cursor);
    let denom = r.dot(normal);
    if denom.abs() < 1e-5 {
        return None;
    }
    let t = (point - eye).dot(normal) / denom;
    (t > 0.0).then(|| eye + r * t)
}

/// the angle of the mouse ray's meeting with the plane through `centre`, measured from `u` towards `v`
pub(super) fn plane_angle(vp: &Mat4, rect: egui::Rect, eye: Vec3, cursor: Pos2, centre: Vec3, normal: Vec3, u: Vec3, v: Vec3) -> Option<f32> {
    let q = ray_plane(vp, rect, eye, cursor, centre, normal)? - centre;
    Some(q.dot(v).atan2(q.dot(u)))
}

fn wrap_angle(mut a: f32) -> f32 {
    while a > std::f32::consts::PI {
        a -= std::f32::consts::TAU;
    }
    while a < -std::f32::consts::PI {
        a += std::f32::consts::TAU;
    }
    a
}

fn snap_to(v: f32, step: f32) -> f32 {
    if step > 0.0 {
        (v / step).round() * step
    } else {
        v
    }
}

/// The world rotation of an entity (its stored rotation is the inverse).
pub(super) fn world_rot(stored: [f32; 4]) -> Quat {
    let q = Quat::from_xyzw(stored[0], stored[1], stored[2], stored[3]);
    if q.length_squared() > 0.0 {
        q.normalize().conjugate()
    } else {
        Quat::IDENTITY
    }
}

pub(super) fn stored_rot(world: Quat) -> [f32; 4] {
    let s = world.normalize().conjugate();
    [s.x, s.y, s.z, s.w]
}

/// What a drag does to the entities it started with. `delta` is the move (world space), `angle` the turn about `axis` through the pivot,
/// `factor` the scale along `axis` (or every way for `uniform`). Pure: the result is the new placements.
pub(super) struct Transform {
    pub delta: Vec3,
    pub rotate: Option<(Vec3, f32)>,
    /// (axis, factor, which scale it changes: 0 xy, 1 z), or a uniform factor
    pub scale: Option<(Vec3, f32, usize)>,
    pub uniform: Option<f32>,
}

pub(super) fn apply_transform(starts: &[(u32, EntityEdit)], pivot: Vec3, t: &Transform) -> Vec<(u32, EntityEdit)> {
    starts
        .iter()
        .map(|(id, s)| {
            let mut e = *s;
            let mut pos = Vec3::from(s.pos);
            if let Some((axis, angle)) = t.rotate {
                let q = Quat::from_axis_angle(axis.normalize_or_zero(), angle);
                pos = pivot + q * (pos - pivot);
                e.rot = stored_rot(q * world_rot(s.rot));
            }
            if let Some((axis, f, which)) = t.scale {
                let a = axis.normalize_or_zero();
                let d = pos - pivot;
                pos = pivot + d + a * (a.dot(d) * (f - 1.0));
                if which == 1 {
                    e.scale_z = (s.scale_z * f).max(0.001);
                } else {
                    e.scale_xy = (s.scale_xy * f).max(0.001);
                }
            }
            if let Some(f) = t.uniform {
                pos = pivot + (pos - pivot) * f;
                e.scale_xy = (s.scale_xy * f).max(0.001);
                e.scale_z = (s.scale_z * f).max(0.001);
            }
            e.pos = (pos + t.delta).to_array();
            (*id, e)
        })
        .collect()
}

impl World {
    /// the middle of the selection and the axes the gizmo uses (the entity's own when one is selected and the space is local)
    fn gizmo_frame(&self, ids: &[u32]) -> Option<(Vec3, Quat)> {
        let placements: Vec<EntityEdit> = ids.iter().filter_map(|&i| self.placement_of(i)).collect();
        if placements.is_empty() {
            return None;
        }
        let pivot = placements.iter().map(|p| Vec3::from(p.pos)).sum::<Vec3>() / placements.len() as f32;
        let frame = if self.space == Space::Local && placements.len() == 1 { world_rot(placements[0].rot) } else { Quat::IDENTITY };
        Some((pivot, frame))
    }

    /// Where the gizmo is: the middle of the selection, the rotation of its axes and the length of an arrow (it stays about the same size on
    /// the screen).
    pub(super) fn gizmo_basis(&self, eye: Vec3) -> Option<(Vec3, Quat, f32)> {
        let (pivot, frame) = self.gizmo_frame(&self.sel)?;
        Some((pivot, frame, ((pivot - eye).length() * 0.13).clamp(1.5, 300.0)))
    }

    /// The handle under the cursor for `tool`, if any.
    fn gizmo_hit(&self, tool: Tool, vp: &Mat4, rect: egui::Rect, cursor: Pos2, pivot: Vec3, axes: [Vec3; 3], len: f32) -> Option<Handle> {
        let o = project(vp, pivot, rect)?;
        if tool == Tool::Move {
            for n in 0..3 {
                let (u, v) = plane_basis(n, axes);
                let corners = [(0.25, 0.25), (0.55, 0.25), (0.55, 0.55), (0.25, 0.55)].map(|(a, b)| project(vp, pivot + (u * a + v * b) * len, rect));
                if let [Some(a), Some(b), Some(c), Some(d)] = corners {
                    if point_in_quad(cursor, [a, b, c, d]) {
                        return Some(Handle::Plane(n));
                    }
                }
            }
        }
        if tool == Tool::Scale && (cursor - o).length() < 10.0 {
            return Some(Handle::Uniform);
        }
        for n in 0..3 {
            let near = match tool {
                Tool::Rotate => {
                    let (u, v) = plane_basis(n, axes);
                    let pts: Vec<Option<Pos2>> = ring_points(pivot, u, v, len).iter().map(|p| project(vp, *p, rect)).collect();
                    pts.windows(2).any(|w| matches!((w[0], w[1]), (Some(a), Some(b)) if dist_to_segment(cursor, a, b) < 9.0))
                }
                _ => project(vp, pivot + axes[n] * len, rect).map_or(false, |tip| dist_to_segment(cursor, o, tip) < 11.0),
            };
            if near {
                return Some(Handle::Axis(n));
            }
        }
        None
    }

    /// Work the gizmo for this frame: start, continue or end a drag, and draw the handles. Returns true while the gizmo has the mouse (a drag
    /// that started on a handle), so the view does not also start a box selection or turn the camera.
    pub(super) fn gizmo(&mut self, ctx: &egui::Context, resp: &egui::Response, painter: &egui::Painter, vp: &Mat4, rect: egui::Rect, eye: Vec3) -> bool {
        self.prune_selection();
        let ids = self.sel.clone();
        let Some((pivot, frame, len)) = self.gizmo_basis(eye) else {
            self.drag = None;
            self.over_gizmo = false;
            return false;
        };
        let axes = [frame * Vec3::X, frame * Vec3::Y, frame * Vec3::Z];
        let cursor = ctx.pointer_interact_pos();
        let (shift, alt) = ctx.input(|i| (i.modifiers.shift, i.modifiers.alt));
        let hovered = if self.drag.is_none() { cursor.filter(|c| rect.contains(*c)).and_then(|c| self.gizmo_hit(self.tool, vp, rect, c, pivot, axes, len)) } else { None };
        self.over_gizmo = hovered.is_some() || self.drag.is_some();
        // ---- start
        if resp.drag_started_by(egui::PointerButton::Primary) && self.drag.is_none() {
            if let (Some(cp), Some(handle)) = (cursor, cursor.and_then(|c| self.gizmo_hit(self.tool, vp, rect, c, pivot, axes, len))) {
                let mut target = ids.clone();
                if shift {
                    // dragging a copy: the copies start where the originals are and are what moves
                    let copies = self.duplicate_selection(Vec3::ZERO);
                    if !copies.is_empty() {
                        target = copies;
                    }
                }
                let starts: Vec<(u32, EntityEdit)> = target.iter().filter_map(|&i| self.placement_of(i).map(|p| (i, p))).collect();
                if !starts.is_empty() {
                    let (t0, hit0) = match (self.tool, handle) {
                        (Tool::Move, Handle::Plane(n)) => (0.0, ray_plane(vp, rect, eye, cp, pivot, axes[n]).unwrap_or(pivot)),
                        (Tool::Rotate, Handle::Axis(n)) => {
                            let (u, v) = plane_basis(n, axes);
                            (plane_angle(vp, rect, eye, cp, pivot, axes[n], u, v).unwrap_or(0.0), pivot)
                        }
                        (_, Handle::Axis(n)) => (axis_param(vp, rect, eye, cp, pivot, axes[n]).unwrap_or(0.0), pivot),
                        _ => (0.0, pivot),
                    };
                    self.drag = Some(Drag { tool: self.tool, handle, starts, pivot, frame, t0, hit0, len, y0: cp.y });
                }
            }
        }
        // ---- continue
        let mut captured = self.drag.is_some();
        if resp.dragged_by(egui::PointerButton::Primary) {
            if let Some(cp) = cursor {
                if let Some(d) = self.drag.take() {
                    let list = self.drag_result(&d, vp, rect, eye, cp, alt);
                    self.drag = Some(d);
                    self.set_placements(&list);
                }
            }
        }
        // ---- end
        if resp.drag_stopped() {
            if self.drag.take().is_some() {
                // the next change is a new undo step; extents and flags are worked out when the map is saved, not while dragging
                self.end_groups();
            }
            captured = false;
        }
        // ---- draw
        let active = self.drag.as_ref().map(|d| d.handle);
        let shown = active.or(hovered);
        let o = project(vp, pivot, rect);
        let alpha_of = |h: Handle| if Some(h) == shown { 255u8 } else { 200 };
        let width_of = |h: Handle| if Some(h) == shown { 5.0 } else { 3.0 };
        for n in 0..3 {
            let col = COLORS[n];
            let h = Handle::Axis(n);
            let c = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha_of(h));
            match self.tool {
                Tool::Rotate => {
                    let (u, v) = plane_basis(n, axes);
                    let pts: Vec<Pos2> = ring_points(pivot, u, v, len).iter().filter_map(|p| project(vp, *p, rect)).collect();
                    if pts.len() > 2 {
                        painter.add(egui::Shape::line(pts, egui::Stroke::new(width_of(h), c)));
                    }
                }
                tool => {
                    if let (Some(a), Some(b)) = (o, project(vp, pivot + axes[n] * len, rect)) {
                        painter.line_segment([a, b], egui::Stroke::new(width_of(h), c));
                        if tool == Tool::Scale {
                            painter.rect_filled(egui::Rect::from_center_size(b, egui::Vec2::splat(if Some(h) == shown { 14.0 } else { 10.0 })), 1.0, c);
                        } else {
                            painter.circle_filled(b, if Some(h) == shown { 7.0 } else { 5.0 }, c);
                        }
                    }
                }
            }
            if tool_is_move(self.tool) {
                let (u, v) = plane_basis(n, axes);
                let corners = [(0.25, 0.25), (0.55, 0.25), (0.55, 0.55), (0.25, 0.55)].map(|(a, b)| project(vp, pivot + (u * a + v * b) * len, rect));
                if let [Some(a), Some(b), Some(c2), Some(d)] = corners {
                    let hp = Handle::Plane(n);
                    let fill = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), if Some(hp) == shown { 150 } else { 70 });
                    painter.add(egui::Shape::convex_polygon(vec![a, b, c2, d], fill, egui::Stroke::new(1.5, c)));
                }
            }
        }
        if self.tool == Tool::Scale {
            if let Some(o) = o {
                let on = shown == Some(Handle::Uniform);
                painter.circle_filled(o, if on { 8.0 } else { 6.0 }, Color32::from_white_alpha(if on { 255 } else { 210 }));
            }
        } else if let Some(o) = o {
            painter.circle_filled(o, 3.0, Color32::from_white_alpha(220));
        }
        captured
    }

    /// The placements a drag gives at the cursor `cp`.
    fn drag_result(&mut self, d: &Drag, vp: &Mat4, rect: egui::Rect, eye: Vec3, cp: Pos2, alt: bool) -> Vec<(u32, EntityEdit)> {
        let axes = [d.frame * Vec3::X, d.frame * Vec3::Y, d.frame * Vec3::Z];
        let world_axes = d.frame == Quat::IDENTITY;
        let grid = if !alt && matches!(self.snap_mode, SnapMode::Grid | SnapMode::Hybrid) { self.snap } else { 0.0 };
        let follow_ground = !alt && matches!(self.snap_mode, SnapMode::Ground | SnapMode::Hybrid);
        let mut t = Transform { delta: Vec3::ZERO, rotate: None, scale: None, uniform: None };
        match (d.tool, d.handle) {
            (Tool::Move, Handle::Axis(n)) => {
                if let Some(p) = axis_param(vp, rect, eye, cp, d.pivot, axes[n]) {
                    let mut delta = p - d.t0;
                    if grid > 0.0 {
                        delta = if world_axes { snap_to(d.pivot[n] + delta, grid) - d.pivot[n] } else { snap_to(delta, grid) };
                    }
                    t.delta = axes[n] * delta;
                    // moving along the ground follows it; moving straight up or down is free (underground stays possible)
                    if follow_ground && axes[n].z.abs() < 0.7 {
                        t.delta = self.follow_ground(d.pivot, t.delta);
                    }
                }
            }
            (Tool::Move, Handle::Plane(n)) => {
                if let Some(hit) = ray_plane(vp, rect, eye, cp, d.pivot, axes[n]) {
                    let mut delta = hit - d.hit0;
                    if grid > 0.0 {
                        let (u, v) = plane_basis(n, axes);
                        if world_axes {
                            let target = d.pivot + delta;
                            let snapped = Vec3::new(snap_to(target.x, grid), snap_to(target.y, grid), snap_to(target.z, grid));
                            // only the in-plane components snap; the one along the normal stays as it was
                            let normal_mask = axes[n].abs();
                            delta = Vec3::new(if normal_mask.x > 0.5 { delta.x } else { snapped.x - d.pivot.x }, if normal_mask.y > 0.5 { delta.y } else { snapped.y - d.pivot.y }, if normal_mask.z > 0.5 { delta.z } else { snapped.z - d.pivot.z });
                        } else {
                            let (a, b) = (snap_to(delta.dot(u), grid), snap_to(delta.dot(v), grid));
                            delta = u * a + v * b;
                        }
                    }
                    t.delta = delta;
                    if follow_ground && axes[n].z.abs() > 0.7 {
                        t.delta = self.follow_ground(d.pivot, t.delta);
                    }
                }
            }
            (Tool::Rotate, Handle::Axis(n)) => {
                let (u, v) = plane_basis(n, axes);
                if let Some(a) = plane_angle(vp, rect, eye, cp, d.pivot, axes[n], u, v) {
                    let mut angle = wrap_angle(a - d.t0);
                    if !alt && self.snap_mode != SnapMode::None {
                        angle = snap_to(angle, self.snap_angle.to_radians());
                    }
                    t.rotate = Some((axes[n], angle));
                }
            }
            (Tool::Scale, Handle::Axis(n)) => {
                if let Some(p) = axis_param(vp, rect, eye, cp, d.pivot, axes[n]) {
                    let mut f = ((d.len + (p - d.t0)) / d.len).clamp(0.05, 50.0);
                    if grid > 0.0 {
                        f = snap_to(f, 0.05).max(0.05);
                    }
                    t.scale = Some((axes[n], f, usize::from(n == 2)));
                }
            }
            (Tool::Scale, Handle::Uniform) => {
                let mut f = ((d.y0 - cp.y) / 150.0).exp().clamp(0.05, 50.0);
                if grid > 0.0 {
                    f = snap_to(f, 0.05).max(0.05);
                }
                t.uniform = Some(f);
            }
            _ => {}
        }
        apply_transform(&d.starts, d.pivot, &t)
    }

    /// `delta` with its vertical part changed so the middle of the selection ends on the ground at the new x and y (when there is ground there)
    fn follow_ground(&mut self, pivot: Vec3, delta: Vec3) -> Vec3 {
        let target = pivot + delta;
        match self.ground_at(target.x, target.y, target.z.max(pivot.z) + 1.0, snap::GroundMode::Below) {
            Some((z, _)) => Vec3::new(delta.x, delta.y, z - pivot.z),
            None => delta,
        }
    }
}

fn tool_is_move(t: Tool) -> bool {
    t == Tool::Move
}

/// 48 points of a circle of radius `0.9 len` in the plane spanned by `u` and `v`
fn ring_points(c: Vec3, u: Vec3, v: Vec3, len: f32) -> Vec<Vec3> {
    (0..=48)
        .map(|k| {
            let a = k as f32 / 48.0 * std::f32::consts::TAU;
            c + (u * a.cos() + v * a.sin()) * len * 0.9
        })
        .collect()
}

/// whether `p` is inside the convex quad `q` (any winding)
fn point_in_quad(p: Pos2, q: [Pos2; 4]) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign.signum() {
                return false;
            }
            sign = cross;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(x: f32, y: f32, z: f32) -> EntityEdit {
        EntityEdit { pos: [x, y, z], rot: [0.0, 0.0, 0.0, 1.0], scale_xy: 1.0, scale_z: 1.0 }
    }

    #[test]
    fn a_move_changes_only_positions() {
        let starts = vec![(1, edit(0.0, 0.0, 0.0)), (2, edit(2.0, 0.0, 0.0))];
        let out = apply_transform(&starts, Vec3::new(1.0, 0.0, 0.0), &Transform { delta: Vec3::new(0.0, 5.0, -3.0), rotate: None, scale: None, uniform: None });
        assert_eq!(out[0].1.pos, [0.0, 5.0, -3.0]);
        assert_eq!(out[1].1.pos, [2.0, 5.0, -3.0], "underground is fine");
        assert_eq!(out[0].1.rot, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn rotating_a_group_turns_the_positions_about_the_middle_and_every_rotation_by_the_same_turn() {
        let starts = vec![(1, edit(0.0, 0.0, 0.0)), (2, edit(2.0, 0.0, 0.0))];
        let pivot = Vec3::new(1.0, 0.0, 0.0);
        let out = apply_transform(&starts, pivot, &Transform { delta: Vec3::ZERO, rotate: Some((Vec3::Z, std::f32::consts::FRAC_PI_2)), scale: None, uniform: None });
        // a quarter turn about Z through (1,0,0): (0,0) -> (1,-1), (2,0) -> (1,1)
        assert!((Vec3::from(out[0].1.pos) - Vec3::new(1.0, -1.0, 0.0)).length() < 1e-4, "{:?}", out[0].1.pos);
        assert!((Vec3::from(out[1].1.pos) - Vec3::new(1.0, 1.0, 0.0)).length() < 1e-4, "{:?}", out[1].1.pos);
        // the world rotation of each is now a quarter turn about Z (the file stores the inverse)
        let w = world_rot(out[0].1.rot);
        assert!((w * Vec3::X - Vec3::Y).length() < 1e-4);
        // a single entity turns in place
        let one = apply_transform(&starts[..1], Vec3::ZERO, &Transform { delta: Vec3::ZERO, rotate: Some((Vec3::Z, 1.0)), scale: None, uniform: None });
        assert_eq!(one[0].1.pos, [0.0, 0.0, 0.0]);
        // turning back restores the stored rotation (to float accuracy)
        let back = apply_transform(&one, Vec3::ZERO, &Transform { delta: Vec3::ZERO, rotate: Some((Vec3::Z, -1.0)), scale: None, uniform: None });
        for k in 0..4 {
            assert!((back[0].1.rot[k] - [0.0, 0.0, 0.0, 1.0][k]).abs() < 1e-5, "{:?}", back[0].1.rot);
        }
    }

    #[test]
    fn scaling_changes_the_right_scale_and_spreads_a_group_along_the_axis() {
        let starts = vec![(1, edit(0.0, 0.0, 0.0)), (2, edit(2.0, 4.0, 0.0))];
        let pivot = Vec3::new(1.0, 2.0, 0.0);
        let out = apply_transform(&starts, pivot, &Transform { delta: Vec3::ZERO, rotate: None, scale: Some((Vec3::X, 2.0, 0)), uniform: None });
        assert_eq!((out[0].1.scale_xy, out[0].1.scale_z), (2.0, 1.0));
        // only the x distance from the middle doubles
        assert_eq!(out[1].1.pos, [3.0, 4.0, 0.0]);
        let z = apply_transform(&starts[..1], pivot, &Transform { delta: Vec3::ZERO, rotate: None, scale: Some((Vec3::Z, 3.0, 1)), uniform: None });
        assert_eq!((z[0].1.scale_xy, z[0].1.scale_z), (1.0, 3.0));
        let u = apply_transform(&starts, pivot, &Transform { delta: Vec3::ZERO, rotate: None, scale: None, uniform: Some(0.5) });
        assert_eq!((u[0].1.scale_xy, u[0].1.scale_z), (0.5, 0.5));
        assert_eq!(u[1].1.pos, [1.5, 3.0, 0.0]);
        // a scale never reaches zero
        let tiny = apply_transform(&starts[..1], pivot, &Transform { delta: Vec3::ZERO, rotate: None, scale: None, uniform: Some(0.0) });
        assert!(tiny[0].1.scale_xy > 0.0);
    }

    #[test]
    fn local_axes_follow_the_entity_and_planes_have_the_right_normals() {
        let frame = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let axes = [frame * Vec3::X, frame * Vec3::Y, frame * Vec3::Z];
        assert!((axes[0] - Vec3::Y).length() < 1e-5);
        for n in 0..3 {
            let (u, v) = plane_basis(n, axes);
            assert!((u.cross(v) - axes[n]).length() < 1e-5, "plane {n} is right-handed");
        }
        assert_eq!(wrap_angle(3.5 * std::f32::consts::PI).round(), (-0.5 * std::f32::consts::PI).round());
        assert_eq!(snap_to(1.26, 0.5), 1.5);
        assert_eq!(snap_to(1.26, 0.0), 1.26);
    }

    #[test]
    fn the_mouse_ray_meets_a_plane_and_a_point_is_found_in_a_quad() {
        let rect = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut cam = FlyCamera::new();
        cam.pos = Vec3::new(0.0, -10.0, 5.0);
        cam.yaw = std::f32::consts::FRAC_PI_2;
        cam.pitch = -0.4;
        let vp = cam.view_proj(800.0 / 600.0);
        // the middle of the screen meets the ground plane in front of the camera
        let hit = ray_plane(&vp, rect, cam.pos, Pos2::new(400.0, 300.0), Vec3::ZERO, Vec3::Z).unwrap();
        assert!(hit.y > -10.0 && hit.z.abs() < 1e-3, "{hit:?}");
        // looking up never meets the ground
        cam.pitch = 0.5;
        let vp = cam.view_proj(800.0 / 600.0);
        assert!(ray_plane(&vp, rect, cam.pos, Pos2::new(400.0, 300.0), Vec3::ZERO, Vec3::Z).is_none());
        let q = [Pos2::new(0.0, 0.0), Pos2::new(10.0, 0.0), Pos2::new(10.0, 10.0), Pos2::new(0.0, 10.0)];
        assert!(point_in_quad(Pos2::new(5.0, 5.0), q) && !point_in_quad(Pos2::new(15.0, 5.0), q));
    }
}
