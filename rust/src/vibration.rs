//! Moteur de courbes de vibration — cœur **pur** (aucune I/O, aucune dépendance).
//!
//! Port de la partie évaluable de `tools/vibration_engine.py` : ajustement
//! polynomial de degré 4 (moindres carrés) d'une courbe d'intensité en fonction
//! de X, utilisé pour l'**aperçu** de la page Vibration. Le format de fichier et
//! la sémantique 2D (condition) vivent côté UI ; ici on ne garde que la
//! régression et l'échantillonnage, qui sont l'asset durable et testable.
//!
//! Choix documenté (comme en Python) : la formule native (`degreeOfcurve:4` dans
//! la DLL) étant hors de portée en RE statique, on la reproduit par une
//! régression de degré 4 sur les points, X normalisé en [-1, 1] pour le
//! conditionnement.

/// Contraint `x` dans `[lo, hi]` (bornes réordonnées si besoin).
pub fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    let (lo, hi) = if lo > hi { (hi, lo) } else { (lo, hi) };
    x.clamp(lo, hi)
}

/// Résout A·x = b par élimination de Gauss (pivot partiel). `None` si singulier.
// Indexation explicite assumée : la mise à jour à deux indices
// `a[r][j] -= facteur * a[col][j]` (r ≠ col) est plus lisible par indices qu'en
// scindant les emprunts de lignes pour un itérateur.
#[allow(clippy::needless_range_loop)]
fn solve_linear(matrix: &[Vec<f64>], rhs: &[f64]) -> Option<Vec<f64>> {
    let n = rhs.len();
    // Matrice augmentée.
    let mut a: Vec<Vec<f64>> = matrix
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut r = row.clone();
            r.push(rhs[i]);
            r
        })
        .collect();
    for col in 0..n {
        // pivot partiel : plus grand |a[r][col]| sous la diagonale
        let pivot = (col..n).max_by(|&r1, &r2| {
            a[r1][col].abs().partial_cmp(&a[r2][col].abs()).unwrap()
        })?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        let inv = 1.0 / a[col][col];
        for j in col..=n {
            a[col][j] *= inv;
        }
        for r in 0..n {
            if r != col && a[r][col] != 0.0 {
                let factor = a[r][col];
                for j in col..=n {
                    a[r][j] -= factor * a[col][j];
                }
            }
        }
    }
    Some((0..n).map(|i| a[i][n]).collect())
}

/// Coefficients `[c0, c1, …, cd]` d'un polynôme de moindres carrés de degré
/// `degree` (ramené à `len-1` s'il y a moins de points). Retombe sur un degré
/// plus bas si le système est singulier ; dernier recours = moyenne constante.
pub fn polyfit_least_squares(xs: &[f64], ys: &[f64], degree: usize) -> Vec<f64> {
    let n = xs.len();
    if n == 0 {
        return vec![0.0];
    }
    let mut deg = degree.min(n - 1) as isize;
    while deg >= 0 {
        let d = deg as usize;
        let m = d + 1;
        let mut powers = vec![0.0; 2 * d + 1];
        let mut rhs = vec![0.0; m];
        for k in 0..n {
            let (xk, yk) = (xs[k], ys[k]);
            let mut xp = 1.0;
            let mut partials = Vec::with_capacity(2 * d + 1);
            for p in powers.iter_mut() {
                *p += xp;
                partials.push(xp);
                xp *= xk;
            }
            for (i, r) in rhs.iter_mut().enumerate() {
                *r += yk * partials[i];
            }
        }
        let normal: Vec<Vec<f64>> = (0..m)
            .map(|i| (0..m).map(|j| powers[i + j]).collect())
            .collect();
        if let Some(sol) = solve_linear(&normal, &rhs) {
            return sol;
        }
        deg -= 1;
    }
    vec![ys.iter().sum::<f64>() / n as f64]
}

/// Évalue le polynôme (Horner) donné par `[c0, c1, …]`.
pub fn polyval(coeffs: &[f64], x: f64) -> f64 {
    coeffs.iter().rev().fold(0.0, |acc, &c| acc * x + c)
}

/// Une courbe d'intensité en fonction de X (régression polynomiale degré 4),
/// X normalisé en [-1, 1] sur l'étendue des points avant l'ajustement.
#[derive(Debug, Clone)]
pub struct Curve {
    coeffs: Vec<f64>,
    x_off: f64,
    x_scale: f64,
}

impl Curve {
    /// Ajuste une courbe de degré `degree` (défaut 4) sur les points `(xs, vs)`.
    pub fn fit(xs: &[f64], vs: &[f64], degree: usize) -> Self {
        let (x0, x1) = if xs.is_empty() {
            (0.0, 1.0)
        } else {
            let lo = xs.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            (lo, hi)
        };
        let x_scale = if x1 > x0 { 2.0 / (x1 - x0) } else { 0.0 };
        let us: Vec<f64> = xs.iter().map(|&x| normalize(x, x0, x_scale)).collect();
        Curve {
            coeffs: polyfit_least_squares(&us, vs, degree),
            x_off: x0,
            x_scale,
        }
    }

    /// Valeur brute de la courbe à `x` (avant clamp sur [v_min, v_max]).
    pub fn value_at(&self, x: f64) -> f64 {
        polyval(&self.coeffs, normalize(x, self.x_off, self.x_scale))
    }
}

fn normalize(x: f64, x_off: f64, x_scale: f64) -> f64 {
    if x_scale == 0.0 {
        0.0
    } else {
        (x - x_off) * x_scale - 1.0
    }
}

/// Échantillonne une courbe pour l'aperçu : `n` points lissés le long de
/// `[min(xs), max(xs)]`, valeurs clampées sur `[v_min, v_max]`. Rend aussi les
/// points d'ancrage réels `(x_i, v_i)` (v clampé). Listes vides si pas de points.
pub fn sample_curve(
    xs: &[f64],
    vs: &[f64],
    v_min: f64,
    v_max: f64,
    n: usize,
) -> (Vec<f64>, Vec<f64>, Vec<(f64, f64)>) {
    if xs.is_empty() || vs.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let n = n.max(2);
    let curve = Curve::fit(xs, vs, 4);
    let lo = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mut sx = Vec::with_capacity(n);
    let mut sv = Vec::with_capacity(n);
    for i in 0..n {
        let x = lo + (hi - lo) * i as f64 / (n - 1) as f64;
        sx.push(x);
        sv.push(clamp(curve.value_at(x), v_min, v_max));
    }
    let anchors = xs
        .iter()
        .zip(vs.iter())
        .map(|(&x, &v)| (x, clamp(v, v_min, v_max)))
        .collect();
    (sx, sv, anchors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_reorders_bounds() {
        assert_eq!(clamp(5.0, 0.0, 10.0), 5.0);
        assert_eq!(clamp(-1.0, 0.0, 10.0), 0.0);
        assert_eq!(clamp(99.0, 10.0, 0.0), 10.0); // bornes inversées
    }

    #[test]
    fn polyval_horner() {
        // 1 + 2x + 3x^2 en x=2 = 1 + 4 + 12 = 17
        assert!((polyval(&[1.0, 2.0, 3.0], 2.0) - 17.0).abs() < 1e-9);
    }

    #[test]
    fn curve_passes_through_points_when_exactly_fitted() {
        // 5 points -> degré 4 -> interpolation exacte
        let xs = [0.0, 1.0, 2.0, 3.0, 4.0];
        let vs = [0.0, 8.0, 30.0, 65.0, 90.0];
        let c = Curve::fit(&xs, &vs, 4);
        for (x, v) in xs.iter().zip(vs.iter()) {
            assert!((c.value_at(*x) - v).abs() < 1e-6, "x={x} attendu {v}");
        }
    }

    #[test]
    fn curve_two_points_is_affine() {
        // 2 points -> degré 1 -> droite
        let c = Curve::fit(&[0.0, 10.0], &[0.0, 100.0], 4);
        assert!((c.value_at(5.0) - 50.0).abs() < 1e-6);
    }

    #[test]
    fn sample_curve_clamps_and_returns_anchors() {
        let xs = [0.0, 80.0, 160.0, 240.0, 300.0];
        let vs = [0.0, 8.0, 30.0, 65.0, 90.0];
        let (sx, sv, anchors) = sample_curve(&xs, &vs, 0.0, 100.0, 64);
        assert_eq!(sx.len(), 64);
        assert_eq!(sv.len(), 64);
        assert_eq!(anchors.len(), 5);
        // bornes d'échantillonnage = étendue des points
        assert!((sx[0] - 0.0).abs() < 1e-9);
        assert!((sx[63] - 300.0).abs() < 1e-9);
        // valeurs dans [0, 100]
        assert!(sv.iter().all(|&v| (0.0..=100.0).contains(&v)));
    }

    #[test]
    fn sample_curve_empty_is_empty() {
        let (sx, sv, a) = sample_curve(&[], &[], 0.0, 100.0, 32);
        assert!(sx.is_empty() && sv.is_empty() && a.is_empty());
    }
}
