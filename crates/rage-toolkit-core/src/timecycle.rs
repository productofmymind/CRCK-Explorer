//! The game's timecycle: for each weather (`w_*.xml`) every light, sky and fog value has one keyframe per hour listed in `time.xml`
//! (13 of them, the last at 22:00 running on to the first at midnight). Only the GLOBAL region is read; the URBAN one adjusts the values inside
//! cities and is left out.
use std::collections::HashMap;

/// the weathers' file names without `w_` and `.xml`, in the order the viewer lists them
pub const WEATHER_FILES: [&str; 15] = ["extrasunny", "clear", "clouds", "overcast", "smog", "foggy", "rain", "thunder", "clearing", "neutral", "snow", "snowlight", "blizzard", "xmas", "halloween"];

/// one weather's keyframes: value name -> a value per hour of the day's samples
#[derive(Clone, Debug, Default)]
pub struct Cycle {
    pub name: String,
    values: HashMap<String, Vec<f32>>,
}

#[derive(Clone, Debug, Default)]
pub struct Timecycle {
    /// the hour of each keyframe
    pub hours: Vec<f32>,
    pub cycles: Vec<Cycle>,
}

/// What the renderer needs at one moment. Colours are the file's linear values (clamped to 0..1); the intensities are already folded into the
/// light colours (see `level`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sky {
    pub sun: [f32; 3],
    pub amb_up: [f32; 3],
    pub amb_down: [f32; 3],
    pub fog: [f32; 3],
    pub fog_density: f32,
    pub zenith: [f32; 3],
    pub zenith_mid: [f32; 3],
    pub zenith_mid_pos: f32,
    pub east: [f32; 3],
    pub west: [f32; 3],
    pub azimuth_mid: [f32; 3],
    pub azimuth_mid_pos: f32,
    pub sun_disc: [f32; 3],
    pub cloud_lit: [f32; 3],
    pub cloud_base: [f32; 3],
    /// The light terms in HDR (colour x intensity, not clamped): the sun, the directional ambient, the natural ambient from above and from
    /// below, and the multiplier of the sky's colours (`sky_hdr`)
    pub sun_hdr: [f32; 3],
    pub amb_dir_hdr: [f32; 3],
    pub nat_up_hdr: [f32; 3],
    pub nat_down_hdr: [f32; 3],
    pub sky_hdr: f32,
    /// the sky's zenith constants as the sky shader reads them: where the zenith blend starts (elevation), and the blend towards the middle
    /// zenith colour on the east and the west side
    pub zenith_blend_start: f32,
    pub zenith_east_blend: f32,
    pub zenith_west_blend: f32,
}

/// The hours of the keyframes from `time.xml`: the `hour` of each `<sample>`.
pub fn parse_hours(text: &str) -> Vec<f32> {
    text.lines()
        .filter(|l| l.contains("<sample"))
        .filter_map(|l| {
            let at = l.find("hour=\"")? + 6;
            l[at..].split('"').next()?.parse().ok()
        })
        .collect()
}

/// One `w_*.xml`: the name of the cycle and the values of its first (GLOBAL) region.
pub fn parse_cycle(text: &str) -> Option<Cycle> {
    let mut name = None;
    let mut values = HashMap::new();
    let mut inside = false;
    for l in text.lines() {
        let l = l.trim();
        if l.starts_with("<cycle") {
            name = l.find("name=\"").and_then(|a| l[a + 6..].split('"').next()).map(str::to_string);
        } else if l.starts_with("<region") {
            if inside {
                break;
            }
            inside = l.contains("name=\"GLOBAL\"");
        } else if inside && l.starts_with('<') && !l.starts_with("</") {
            // <tag> v v v </tag>
            let (Some(open_end), Some(close)) = (l.find('>'), l.rfind("</")) else { continue };
            if close <= open_end {
                continue;
            }
            let nums: Vec<f32> = l[open_end + 1..close].split_whitespace().filter_map(|t| t.parse().ok()).collect();
            if !nums.is_empty() {
                values.insert(l[1..open_end].to_string(), nums);
            }
        }
    }
    Some(Cycle { name: name?, values })
}

/// How strong a light of `value` looks next to the brightest it gets (`reference`). The game maps these through an exposure and a filmic
/// tone curve this viewer does not have; a root keeps dawn and dusk visible next to the noon sun, which is 64 times stronger on paper.
fn level(value: f32, reference: f32) -> f32 {
    (value / reference).max(0.0).powf(0.4).min(1.25)
}

impl Timecycle {
    pub fn is_empty(&self) -> bool {
        self.cycles.is_empty() || self.hours.len() < 2
    }

    pub fn names(&self) -> Vec<&str> {
        self.cycles.iter().map(|c| c.name.as_str()).collect()
    }

    /// one value at `hour` (0..24): linear between the two keyframes round it; after the last keyframe it runs on to the first
    fn value(&self, cycle: &Cycle, tag: &str, hour: f32) -> f32 {
        let Some(v) = cycle.values.get(tag) else { return 0.0 };
        let n = self.hours.len().min(v.len());
        if n < 2 {
            return v.first().copied().unwrap_or(0.0);
        }
        let hour = hour.rem_euclid(24.0);
        // the keyframe at or before `hour`; before the first one the day's last keyframe (the evening before) holds
        let i = (0..n).rev().find(|&k| self.hours[k] <= hour).unwrap_or(n - 1);
        let j = (i + 1) % n;
        let (h0, h1) = (self.hours[i], if j == 0 { self.hours[0] + 24.0 } else { self.hours[j] });
        let h = if hour < h0 { hour + 24.0 } else { hour };
        let t = if h1 > h0 { ((h - h0) / (h1 - h0)).clamp(0.0, 1.0) } else { 0.0 };
        v[i] + (v[j] - v[i]) * t
    }

    fn colour(&self, c: &Cycle, prefix: &str, hour: f32) -> [f32; 3] {
        ["r", "g", "b"].map(|ch| self.value(c, &format!("{prefix}_{ch}"), hour).clamp(0.0, 1.0))
    }

    /// The light, sky and fog of weather `weather` (an index into `cycles`) at `hour`.
    pub fn sample(&self, weather: usize, hour: f32) -> Option<Sky> {
        let c = self.cycles.get(weather)?;
        if self.hours.len() < 2 {
            return None;
        }
        let scale = |col: [f32; 3], k: f32| col.map(|x| x * k);
        let sun = scale(self.colour(c, "light_dir_col", hour), 0.62 * level(self.value(c, "light_dir_mult", hour), 64.0));
        let amb = |dir: &str| {
            let k = 0.5 * level(self.value(c, &format!("light_natural_amb_{dir}_intensity"), hour), 5.0);
            scale(self.colour(c, &format!("light_natural_amb_{dir}_col"), hour), k)
        };
        // a value that may be missing from a file counts as 1 (the intensity multipliers)
        let mult = |tag: &str| if c.values.contains_key(tag) { self.value(c, tag, hour) } else { 1.0 };
        let hdr = |prefix: &str, intensity: &str, extra: f32| scale(self.colour(c, prefix, hour), self.value(c, intensity, hour) * extra);
        Some(Sky {
            sun_hdr: scale(self.colour(c, "light_dir_col", hour), self.value(c, "light_dir_mult", hour)),
            amb_dir_hdr: hdr("light_directional_amb_col", "light_directional_amb_intensity", mult("light_directional_amb_intensity_mult")),
            nat_up_hdr: hdr("light_natural_amb_up_col", "light_natural_amb_up_intensity", mult("light_natural_amb_up_intensity_mult")),
            nat_down_hdr: hdr("light_natural_amb_down_col", "light_natural_amb_down_intensity", 1.0),
            sky_hdr: self.value(c, "sky_hdr", hour),
            zenith_blend_start: self.value(c, "sky_zenith_blend_start", hour),
            zenith_east_blend: self.value(c, "sky_zenith_transition_east_blend", hour),
            zenith_west_blend: self.value(c, "sky_zenith_transition_west_blend", hour),
            sun,
            amb_up: amb("up"),
            amb_down: amb("down"),
            fog: self.colour(c, "fog_col", hour),
            fog_density: self.value(c, "fog_density", hour),
            zenith: self.colour(c, "sky_zenith_col", hour),
            zenith_mid: self.colour(c, "sky_zenith_transition_col", hour),
            zenith_mid_pos: self.value(c, "sky_zenith_transition_position", hour).clamp(0.05, 0.95),
            east: self.colour(c, "sky_azimuth_east_col", hour),
            west: self.colour(c, "sky_azimuth_west_col", hour),
            azimuth_mid: self.colour(c, "sky_azimuth_transition_col", hour),
            azimuth_mid_pos: self.value(c, "sky_azimuth_transition_position", hour).clamp(0.05, 0.95),
            sun_disc: self.colour(c, "sky_sun_col", hour),
            cloud_lit: self.colour(c, "sky_cloud_mid_col", hour),
            cloud_base: self.colour(c, "sky_cloud_base_col", hour),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CYCLE: &str = "<?xml?>\n<timecycle_keyframe_data>\n  <cycle name=\"TEST\" regions=\"2\">\n    <region name=\"GLOBAL\">\n      <light_dir_col_r> 0.0 0.5 1.0 </light_dir_col_r>\n      <light_dir_mult> 0.0 64.0 0.0 </light_dir_mult>\n    </region>\n    <region name=\"URBAN\">\n      <light_dir_col_r> 9.0 9.0 9.0 </light_dir_col_r>\n    </region>\n  </cycle>\n";

    fn tc() -> Timecycle {
        Timecycle { hours: vec![0.0, 12.0, 22.0], cycles: vec![parse_cycle(CYCLE).unwrap()] }
    }

    #[test]
    fn reads_hours_and_the_global_region() {
        assert_eq!(parse_hours("<sample name=\"00:00\" hour=\"0\" duration=\"4\"/>\n<sample name=\"09:00\" hour=\"10\"/>\n<region name=\"URBAN\"/>"), vec![0.0, 10.0]);
        let c = parse_cycle(CYCLE).unwrap();
        assert_eq!(c.name, "TEST");
        assert_eq!(c.values["light_dir_col_r"], vec![0.0, 0.5, 1.0]);
        // the URBAN region's value is not the one kept
        assert_eq!(c.values.len(), 2);
    }

    #[test]
    fn values_run_between_keyframes_and_wrap_at_midnight() {
        let t = tc();
        let c = &t.cycles[0];
        assert!((t.value(c, "light_dir_col_r", 6.0) - 0.25).abs() < 1e-5);
        assert!((t.value(c, "light_dir_col_r", 12.0) - 0.5).abs() < 1e-5);
        // 22:00 is 1.0 and midnight 0.0: 23:00 is half way
        assert!((t.value(c, "light_dir_col_r", 23.0) - 0.5).abs() < 1e-5);
        assert!((t.value(c, "light_dir_col_r", 24.0) - 0.0).abs() < 1e-5);
        assert_eq!(t.value(c, "no_such_value", 3.0), 0.0);
    }

    #[test]
    fn the_noon_sun_is_stronger_than_dusk_and_night_is_dark() {
        let t = tc();
        let noon = t.sample(0, 12.0).unwrap();
        let dusk = t.sample(0, 21.0).unwrap();
        assert!(noon.sun[0] > dusk.sun[0]);
        assert_eq!(t.sample(0, 0.0).unwrap().sun, [0.0, 0.0, 0.0]);
        assert!(t.sample(3, 12.0).is_none());
    }

    /// the game's own files (copied to target/scratch by hand); skipped when they are not there
    #[test]
    fn real_files_give_a_blue_noon_and_an_orange_sunset() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/scratch");
        let (Ok(time), Ok(cycle)) = (std::fs::read_to_string(dir.join("time.xml")), std::fs::read_to_string(dir.join("w_extrasunny.xml"))) else { return };
        let t = Timecycle { hours: parse_hours(&time), cycles: vec![parse_cycle(&cycle).unwrap()] };
        assert_eq!(t.hours.len(), 13);
        let noon = t.sample(0, 12.0).unwrap();
        assert!(noon.east[2] > noon.east[0], "the noon sky is blue: {:?}", noon.east);
        let dusk = t.sample(0, 20.0).unwrap();
        assert!(dusk.fog[0] > dusk.fog[2], "the dusk fog is warm: {:?}", dusk.fog);
        assert!(noon.sun[0] > dusk.sun[0] * 0.5 && t.sample(0, 0.0).unwrap().sun[0] < 0.1);
    }

    #[test]
    fn the_hdr_light_terms_are_colour_times_intensity() {
        let text = "<cycle name=\"T\">
<region name=\"GLOBAL\">
<light_dir_col_r> 1.0 </light_dir_col_r>
<light_dir_col_g> 0.5 </light_dir_col_g>
<light_dir_col_b> 0.0 </light_dir_col_b>
<light_dir_mult> 64.0 </light_dir_mult>
<light_natural_amb_up_col_r> 0.5 </light_natural_amb_up_col_r>
<light_natural_amb_up_col_g> 0.5 </light_natural_amb_up_col_g>
<light_natural_amb_up_col_b> 0.5 </light_natural_amb_up_col_b>
<light_natural_amb_up_intensity> 4.0 </light_natural_amb_up_intensity>
<sky_hdr> 38.0 </sky_hdr>
</region>
";
        let t = Timecycle { hours: vec![0.0, 12.0], cycles: vec![parse_cycle(text).unwrap()] };
        let s = t.sample(0, 6.0).unwrap();
        assert_eq!(s.sun_hdr, [64.0, 32.0, 0.0]);
        // the up multiplier is missing from the file: it counts as 1
        assert_eq!(s.nat_up_hdr, [2.0, 2.0, 2.0]);
        assert_eq!(s.sky_hdr, 38.0);
        // the directional ambient has no intensity in this file: nothing
        assert_eq!(s.amb_dir_hdr, [0.0, 0.0, 0.0]);
    }

    /// The game's own extra-sunny file at `target/scratch` (the user's own game file): the noon sun is about 64 units, the sky 38.
    #[test]
    fn the_real_noon_has_a_64_unit_sun_and_a_sky_multiplier_of_38() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/scratch");
        let (Ok(time), Ok(cycle)) = (std::fs::read_to_string(dir.join("time.xml")), std::fs::read_to_string(dir.join("w_extrasunny.xml"))) else { return };
        let t = Timecycle { hours: parse_hours(&time), cycles: vec![parse_cycle(&cycle).unwrap()] };
        let noon = t.sample(0, 12.0).unwrap();
        assert!(noon.sun_hdr[0] > 55.0 && noon.sun_hdr[0] <= 64.5, "{:?}", noon.sun_hdr);
        assert!((noon.sky_hdr - 38.0).abs() < 0.01);
        assert!(noon.nat_down_hdr[2] > noon.nat_down_hdr[0], "the light from below is blue");
        assert!(t.sample(0, 0.0).unwrap().sun_hdr[0] < 0.5, "no sun at midnight");
    }
}

