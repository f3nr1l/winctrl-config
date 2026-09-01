//! Courbe de réponse et inversion d'un axe — couche uinput.
//!
//! L'inversion d'axe et la courbe de réponse ne sont pas écrites dans le manche.
//! La propriété `is_reversed` et toute la structure `CurveData` (`curve_type`,
//! `lower/upper`, `center_*`, `curve`, `zoom`, `x_pos/y_pos`, `rotate*`) sont
//! appliquées côté OS : la transformation s'exécute dans [`crate::remap`] au
//! moment de ré-émettre l'axe sur le périphérique virtuel — donc réversible,
//! sans écriture matérielle.
//!
//! Le moteur ([`CurveData::apply`]) est **pur** : testable sans matériel.
//!
//! ## Modèle (reconstruction documentée)
//! Valeur brute `v ∈ [min, max]` → normalisée `u ∈ [0, 1]`, puis, dans l'ordre :
//! 1. **inversion** (`is_reversed`) : `u ← 1 − u` ;
//! 2. **bornes** : `lower` % de course morte au minimum, `upper` % de saturation
//!    au maximum, ré-étirés sur `[0, 1]` ;
//! 3. **deadzone centrale** (`center_lower`/`center_upper`, surtout axes centrés) :
//!    plage morte autour du milieu, chaque moitié ré-étirée ;
//! 4. **courbe** (`curve_type`, `curve`) sur `s = 2u − 1 ∈ [−1, 1]` : `J` = expo
//!    simple, `S` = expo réfléchie aux deux extrémités (selle au centre + aux
//!    bords). `curve > 0` = réponse plus douce au centre ;
//! 5. **gain** (`zoom`) : facteur multiplicatif borné.
//!
//! `x_pos`/`y_pos` = **point de contrôle** unique (Bézier quadratique de `(0,0)`
//! à `(1,1)` passant par `(x_pos, y_pos)`) appliqué après la courbe S/J ; neutre à
//! `(50, 50)`. `rotate`/`rotate_group` = **rotation d'un couple d'axes** (X/Y ou
//! Rx/Ry) : elle couple deux axes et n'est donc **pas** dans [`CurveData::apply`]
//! (qui est mono-axe) mais dans la couche uinput ([`rotate_pair`]).

/// Forme de la courbe de réponse. `S` (sigmoïde) et `J` (exponentielle) sont les
/// deux seuls choix de SimApp Pro (`is_reversed_Option`… `curve_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CurveType {
    /// Courbe en S : plate au centre **et** aux extrêmes (pente maximale à
    /// mi-course). C'est le défaut de SimApp Pro.
    #[default]
    S,
    /// Courbe en J : exponentielle simple (progressive).
    J,
}

impl CurveType {
    pub fn as_str(self) -> &'static str {
        match self {
            CurveType::S => "S",
            CurveType::J => "J",
        }
    }

    pub fn from_label(s: &str) -> Self {
        match s {
            "J" => CurveType::J,
            _ => CurveType::S,
        }
    }
}

/// Réglages de réponse d'un axe, calqués sur le `CurveData` de SimApp Pro (mêmes
/// champs, mêmes plages). `Default` = **identité** (aucune transformation).
///
/// Unités entières comme dans l'appli d'origine : pourcentages `0..=50` pour les
/// bornes/deadzones, `curve ∈ −18..=18`, `zoom ∈ −10..=10`, `x/y_pos ∈ 1..=99`,
/// `rotate ∈ −25..=25`. Les valeurs hors plage sont **bornées** par [`Self::apply`]
/// (jamais paniquer sur une config disque douteuse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveData {
    pub curve_type: CurveType,
    pub is_reversed: bool,
    /// Course morte au minimum, en % (`0..=50`).
    pub lower: u8,
    /// Saturation au maximum, en % (`0..=50`).
    pub upper: u8,
    /// Deadzone centrale côté bas, en % (`0..=50`).
    pub center_lower: u8,
    /// Deadzone centrale côté haut, en % (`0..=50`).
    pub center_upper: u8,
    /// Intensité de courbure (`−18..=18`, `0` = linéaire).
    pub curve: i8,
    /// Gain (`−10..=10`, `0` = ×1).
    pub zoom: i8,
    /// Point de contrôle X (`1..=99`) — stocké, non appliqué (phase 2).
    pub x_pos: u8,
    /// Point de contrôle Y (`1..=99`) — stocké, non appliqué (phase 2).
    pub y_pos: u8,
    /// Rotation d'axes couplés (`−25..=25`) — stocké, non appliqué (phase 2).
    pub rotate: i8,
    /// Masque des axes couplés — stocké, non appliqué (phase 2).
    pub rotate_group: u32,
}

impl Default for CurveData {
    fn default() -> Self {
        CurveData {
            curve_type: CurveType::S,
            is_reversed: false,
            lower: 0,
            upper: 0,
            center_lower: 0,
            center_upper: 0,
            curve: 0,
            zoom: 0,
            x_pos: 50,
            y_pos: 50,
            rotate: 0,
            rotate_group: 0,
        }
    }
}

impl CurveData {
    /// `true` si la courbe ne change **rien** : ni inversion, ni bornes, ni
    /// deadzone, ni courbure, ni gain. Permet à [`crate::remap`] de router l'axe
    /// tel quel (aucun coût) quand l'utilisateur n'a rien réglé.
    pub fn is_identity(&self) -> bool {
        !self.is_reversed
            && self.lower == 0
            && self.upper == 0
            && self.center_lower == 0
            && self.center_upper == 0
            && self.curve == 0
            && self.zoom == 0
            && self.x_pos == 50
            && self.y_pos == 50
        // `rotate` couple DEUX axes : il est appliqué dans la couche uinput
        // ([`rotate_pair`]), pas dans `apply`. Un axe qui n'a QUE `rotate ≠ 0` est
        // donc « identité » ici, mais reste routé via son groupe de rotation.
    }

    /// `true` si l'axe participe à une rotation de couple (`rotate ≠ 0` et un
    /// `rotate_group` non nul).
    pub fn has_rotation(&self) -> bool {
        self.rotate != 0 && self.rotate_group != 0
    }

    /// Applique la courbe à une valeur brute `raw ∈ [min, max]` et renvoie la
    /// valeur transformée, **bornée** à `[min, max]`. `min == max` (axe dégénéré)
    /// renvoie `raw` inchangé.
    pub fn apply(&self, raw: i32, min: i32, max: i32) -> i32 {
        if min >= max {
            return raw;
        }
        let span = (max - min) as f64;
        let mut u = (raw - min) as f64 / span;
        u = u.clamp(0.0, 1.0);

        if self.is_reversed {
            u = 1.0 - u;
        }

        u = apply_bounds(u, pct(self.lower), pct(self.upper));
        u = apply_center_deadzone(u, pct(self.center_lower), pct(self.center_upper));

        // Courbure sur l'axe signé s ∈ [−1, 1].
        let mut s = 2.0 * u - 1.0;
        s = shape(s, self.curve_type, curve_k(self.curve));

        // Gain (zoom) : facteur linéaire 1 + zoom/10 ∈ [0, 2], borné.
        s *= 1.0 + f64::from(self.zoom.clamp(-10, 10)) / 10.0;
        s = s.clamp(-1.0, 1.0);

        // Point de contrôle (Bézier) sur la valeur normalisée [0, 1].
        let mut u_out = (s + 1.0) / 2.0;
        u_out = control_point(u_out, self.x_pos, self.y_pos);

        let v = min as f64 + u_out.clamp(0.0, 1.0) * span;
        (v.round() as i32).clamp(min, max)
    }
}

/// Rotation d'un **couple** d'axes (X/Y, Rx/Ry…) autour de leur centre, de
/// `angle_deg` degrés. Chaque axe est d'abord ramené à un signé `[-1, 1]` autour de
/// son milieu `(min+max)/2`, la paire est tournée, puis re-dénormalisée et bornée.
/// Fonction **pure** : la couche uinput fournit les dernières valeurs des deux axes.
pub fn rotate_pair(
    a_raw: i32,
    a_min: i32,
    a_max: i32,
    b_raw: i32,
    b_min: i32,
    b_max: i32,
    angle_deg: f64,
) -> (i32, i32) {
    if a_min >= a_max || b_min >= b_max || angle_deg == 0.0 {
        return (a_raw, b_raw);
    }
    let to_signed = |raw: i32, mn: i32, mx: i32| {
        let u = (raw - mn) as f64 / (mx - mn) as f64;
        (2.0 * u - 1.0).clamp(-1.0, 1.0)
    };
    let from_signed = |s: f64, mn: i32, mx: i32| {
        let u = ((s.clamp(-1.0, 1.0)) + 1.0) / 2.0;
        (mn as f64 + u * (mx - mn) as f64).round() as i32
    };
    let (sx, sy) = (to_signed(a_raw, a_min, a_max), to_signed(b_raw, b_min, b_max));
    let th = angle_deg.to_radians();
    let (c, s) = (th.cos(), th.sin());
    let rx = sx * c - sy * s;
    let ry = sx * s + sy * c;
    (from_signed(rx, a_min, a_max), from_signed(ry, b_min, b_max))
}

/// Point de contrôle unique : Bézier quadratique `P0=(0,0)`, `P1=(cx,cy)`,
/// `P2=(1,1)` avec `cx=x_pos/100`, `cy=y_pos/100`. Résout `x(s)=u` puis rend `y(s)`.
/// Neutre (identité) quand le point est au centre `(50, 50)`.
fn control_point(u: f64, x_pos: u8, y_pos: u8) -> f64 {
    let cx = f64::from(x_pos.clamp(1, 99)) / 100.0;
    let cy = f64::from(y_pos.clamp(1, 99)) / 100.0;
    if (cx - 0.5).abs() < 1e-9 && (cy - 0.5).abs() < 1e-9 {
        return u;
    }
    // x(s) = (1-2cx) s² + 2cx s = u  →  résoudre pour s ∈ [0, 1].
    let a = 1.0 - 2.0 * cx;
    let s = if a.abs() < 1e-9 {
        u // cx = 0.5 : x(s) = s
    } else {
        let disc = (2.0 * cx) * (2.0 * cx) + 4.0 * a * u;
        let root = (-2.0 * cx + disc.max(0.0).sqrt()) / (2.0 * a);
        root.clamp(0.0, 1.0)
    };
    // y(s) = (1-2cy) s² + 2cy s.
    ((1.0 - 2.0 * cy) * s + 2.0 * cy) * s
}

/// Pourcentage `0..=50` → fraction `0.0..=0.5`.
fn pct(v: u8) -> f64 {
    f64::from(v.min(50)) / 100.0
}

/// `curve ∈ −18..=18` → intensité normalisée `k ∈ [−1, 1]`.
fn curve_k(curve: i8) -> f64 {
    f64::from(curve.clamp(-18, 18)) / 18.0
}

/// Ré-étire `[lo, 1 − hi_margin]` sur `[0, 1]` (course morte basse, saturation
/// haute). `lo + hi_margin ≥ 1` (réglages absurdes) : renvoie une rampe plate.
fn apply_bounds(u: f64, lo: f64, hi_margin: f64) -> f64 {
    let hi = 1.0 - hi_margin;
    if hi <= lo {
        return 0.0;
    }
    ((u - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// Deadzone centrale autour de `0.5` : écrase `[0.5 − cl, 0.5 + cu]` sur `0.5` et
/// ré-étire chaque moitié sur `[0, 0.5]` / `[0.5, 1]`.
fn apply_center_deadzone(u: f64, cl: f64, cu: f64) -> f64 {
    if cl <= 0.0 && cu <= 0.0 {
        return u;
    }
    let low_edge = 0.5 - cl;
    let high_edge = 0.5 + cu;
    if u < low_edge {
        // [0, low_edge] → [0, 0.5]
        if low_edge <= 0.0 {
            0.0
        } else {
            u * 0.5 / low_edge
        }
    } else if u > high_edge {
        // [high_edge, 1] → [0.5, 1]
        let top = 1.0 - high_edge;
        if top <= 0.0 {
            1.0
        } else {
            0.5 + (u - high_edge) * 0.5 / top
        }
    } else {
        0.5
    }
}

/// Fonction de forme impaire, monotone, bornée `[−1, 1]`, appliquée à `s ∈ [−1, 1]`.
///
/// `k ∈ [−1, 1]` : `k = 0` → identité. `p = 3^k ∈ [1/3, 3]` est l'exposant.
/// - **J** : `sign(s)·|s|^p` — expo simple (p>1 : doux au centre).
/// - **S** : expo réfléchie autour de `±0.5` — plate au centre *et* aux bords
///   quand p>1 (vraie forme en S), raide au centre quand p<1.
fn shape(s: f64, ct: CurveType, k: f64) -> f64 {
    if k == 0.0 {
        return s;
    }
    let p = 3f64.powf(k);
    let sign = if s < 0.0 { -1.0 } else { 1.0 };
    let a = s.abs().min(1.0);
    let out = match ct {
        CurveType::J => a.powf(p),
        CurveType::S => {
            if a <= 0.5 {
                0.5 * (2.0 * a).powf(p)
            } else {
                1.0 - 0.5 * (2.0 * (1.0 - a)).powf(p)
            }
        }
    };
    sign * out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i32 = 0;
    const MAX: i32 = 4095; // slider/throttle

    #[test]
    fn default_is_identity() {
        let c = CurveData::default();
        assert!(c.is_identity());
        for &v in &[0, 1, 1000, 2048, 4095] {
            assert_eq!(c.apply(v, MIN, MAX), v);
        }
    }

    #[test]
    fn reverse_flips_endpoints_and_midpoint() {
        let c = CurveData {
            is_reversed: true,
            ..Default::default()
        };
        assert!(!c.is_identity());
        assert_eq!(c.apply(MIN, MIN, MAX), MAX);
        assert_eq!(c.apply(MAX, MIN, MAX), MIN);
        // milieu invariant par symétrie
        assert_eq!(c.apply(2048, MIN, MAX), 4095 - 2048);
    }

    #[test]
    fn reverse_is_monotone_decreasing() {
        let c = CurveData {
            is_reversed: true,
            ..Default::default()
        };
        let mut prev = c.apply(MIN, MIN, MAX);
        for v in (0..=4095).step_by(37) {
            let out = c.apply(v, MIN, MAX);
            assert!(out <= prev + 1, "doit décroître : {v} -> {out} (prev {prev})");
            prev = out;
        }
    }

    #[test]
    fn lower_deadzone_holds_bottom_then_rises() {
        // 20 % de course morte en bas.
        let c = CurveData {
            lower: 20,
            ..Default::default()
        };
        assert_eq!(c.apply(MIN, MIN, MAX), MIN); // dans la zone morte
        assert_eq!(c.apply((0.20 * 4095.0) as i32, MIN, MAX), MIN); // bord de zone
        assert_eq!(c.apply(MAX, MIN, MAX), MAX); // plein toujours atteint
        // à mi-course physique (0.5), sortie ≈ (0.5-0.2)/0.8 = 0.375
        let mid = c.apply(2048, MIN, MAX) as f64 / 4095.0;
        assert!((mid - 0.375).abs() < 0.01, "mid={mid}");
    }

    #[test]
    fn upper_saturation_reaches_full_early() {
        let c = CurveData {
            upper: 20,
            ..Default::default()
        };
        // à 80 % de course physique, déjà plein.
        assert_eq!(c.apply((0.80 * 4095.0) as i32, MIN, MAX), MAX);
        assert_eq!(c.apply(MIN, MIN, MAX), MIN);
    }

    #[test]
    fn center_deadzone_centered_axis() {
        // axe centré [-1..1] simulé par min<0.
        let (mn, mx) = (-32768, 32767);
        let c = CurveData {
            center_lower: 10,
            center_upper: 10,
            ..Default::default()
        };
        let center = 0; // milieu
        let out = c.apply(center, mn, mx) as f64;
        let norm = (out - mn as f64) / (mx as f64 - mn as f64);
        assert!((norm - 0.5).abs() < 0.01, "centre doit rester au milieu: {norm}");
        // extrêmes conservés
        assert_eq!(c.apply(mn, mn, mx), mn);
        assert_eq!(c.apply(mx, mn, mx), mx);
    }

    #[test]
    fn curve_endpoints_and_center_invariant() {
        // Plage PAIRE pour un centre exact (2048 = milieu de [0, 4096]) : une
        // courbe fixe toujours les extrémités et le centre.
        let (mn, mx, ctr) = (0, 4096, 2048);
        for &ct in &[CurveType::S, CurveType::J] {
            for curve in [-18i8, -9, 9, 18] {
                let c = CurveData {
                    curve_type: ct,
                    curve,
                    ..Default::default()
                };
                assert_eq!(c.apply(mn, mn, mx), mn, "min {ct:?} {curve}");
                assert_eq!(c.apply(mx, mn, mx), mx, "max {ct:?} {curve}");
                assert_eq!(c.apply(ctr, mn, mx), ctr, "centre {ct:?} {curve}");
            }
        }
    }

    #[test]
    fn curve_is_monotone() {
        for &ct in &[CurveType::S, CurveType::J] {
            for curve in [-18i8, -5, 5, 18] {
                let c = CurveData {
                    curve_type: ct,
                    curve,
                    ..Default::default()
                };
                let mut prev = -1;
                for v in (0..=4095).step_by(23) {
                    let out = c.apply(v, MIN, MAX);
                    assert!(out >= prev, "monotone {ct:?} {curve}: {v}->{out} < {prev}");
                    prev = out;
                }
            }
        }
    }

    #[test]
    fn positive_curve_softens_center_response() {
        // curve>0 : près du centre, la réponse est tirée VERS le centre (pente
        // plus faible). L'effet est symétrique → on mesure la distance au centre.
        let center = 2048;
        let lin = CurveData::default().apply(1024, MIN, MAX);
        let j = CurveData {
            curve_type: CurveType::J,
            curve: 12,
            ..Default::default()
        }
        .apply(1024, MIN, MAX);
        assert!(
            (j - center).abs() < (lin - center).abs(),
            "J curve>0 doit adoucir près du centre: {j} vs {lin}"
        );
    }

    #[test]
    fn zoom_gain_amplifies_then_clamps() {
        // Plage paire pour un centre exact.
        let (mn, mx, ctr) = (0, 4096, 2048);
        let c = CurveData {
            zoom: 10, // ×2
            ..Default::default()
        };
        // à 75 % : s=0.5, ×2 -> 1.0 -> plein.
        assert_eq!(c.apply(3 * mx / 4, mn, mx), mx);
        // extrémités et centre restent bornés/invariants
        assert_eq!(c.apply(mn, mn, mx), mn);
        assert_eq!(c.apply(ctr, mn, mx), ctr);
    }

    #[test]
    fn out_of_range_config_does_not_panic() {
        let c = CurveData {
            lower: 200,
            upper: 200,
            center_lower: 99,
            center_upper: 99,
            curve: 127,
            zoom: 127,
            ..Default::default()
        };
        for v in [MIN, 2048, MAX] {
            let out = c.apply(v, MIN, MAX);
            assert!((MIN..=MAX).contains(&out));
        }
    }

    #[test]
    fn degenerate_axis_returns_raw() {
        let c = CurveData {
            is_reversed: true,
            ..Default::default()
        };
        assert_eq!(c.apply(1234, 5, 5), 1234);
    }

    // --- Phase 2 : point de contrôle -------------------------------------
    #[test]
    fn control_point_default_is_identity() {
        assert!(CurveData::default().is_identity());
        // Point centré (50/50) = identité.
        let c = CurveData {
            x_pos: 50,
            y_pos: 50,
            ..Default::default()
        };
        assert!(c.is_identity());
        for v in (0..=4096).step_by(64) {
            assert_eq!(c.apply(v, 0, 4096), v);
        }
    }

    #[test]
    fn control_point_off_center_is_not_identity_but_fixes_ends() {
        let c = CurveData {
            x_pos: 25,
            y_pos: 75, // point tiré vers le haut-gauche
            ..Default::default()
        };
        assert!(!c.is_identity());
        assert_eq!(c.apply(0, 0, 4096), 0);
        assert_eq!(c.apply(4096, 0, 4096), 4096);
        // à mi-course, la sortie est tirée VERS LE HAUT (y_pos > x_pos).
        assert!(c.apply(2048, 0, 4096) > 2048);
    }

    #[test]
    fn control_point_is_monotone() {
        for (x, y) in [(20u8, 80u8), (80, 20), (10, 40), (90, 60)] {
            let c = CurveData {
                x_pos: x,
                y_pos: y,
                ..Default::default()
            };
            let mut prev = -1;
            for v in (0..=4096).step_by(31) {
                let out = c.apply(v, 0, 4096);
                assert!(out >= prev, "monotone point({x},{y}): {v}->{out}<{prev}");
                prev = out;
            }
        }
    }

    // --- Phase 2 : rotation de couple ------------------------------------
    #[test]
    fn rotate_zero_is_identity() {
        assert_eq!(rotate_pair(100, 0, 200, 40, 0, 200, 0.0), (100, 40));
    }

    #[test]
    fn rotate_center_stays_center() {
        // centre (milieu de plage) invariant par rotation.
        let (mn, mx) = (-1000, 1000);
        assert_eq!(rotate_pair(0, mn, mx, 0, mn, mx, 25.0), (0, 0));
    }

    #[test]
    fn rotate_90_maps_axes() {
        // plage symétrique ; +90° : (x,y)=(max,centre) -> (centre,max).
        let (mn, mx) = (-1000, 1000);
        let (a, b) = rotate_pair(mx, mn, mx, 0, mn, mx, 90.0);
        assert!(a.abs() <= 1, "a={a}");
        assert!((b - mx).abs() <= 1, "b={b}");
    }

    #[test]
    fn rotate_stays_bounded() {
        let (mn, mx) = (-1000, 1000);
        for &ang in &[-25.0, -10.0, 10.0, 25.0] {
            for &x in &[mn, -300, 0, 500, mx] {
                for &y in &[mn, 0, mx] {
                    let (a, b) = rotate_pair(x, mn, mx, y, mn, mx, ang);
                    assert!((mn..=mx).contains(&a) && (mn..=mx).contains(&b));
                }
            }
        }
    }

    #[test]
    fn rotate_degenerate_axis_returns_raw() {
        assert_eq!(rotate_pair(5, 10, 10, 7, 0, 100, 20.0), (5, 7));
    }
}
