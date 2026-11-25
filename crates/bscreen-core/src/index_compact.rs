//! L'index compact des vecteurs (sens et images, 24/11/2026).
//!
//! Garder tous les vecteurs en mémoire (768 octets chacun) ne tenait pas : 76 528 vecteurs de sens
//! en 4 jours, 1,3 Go à 90 jours, et l'archive garde l'index pour toujours. En mémoire ne reste
//! que le signe de chaque nombre (768 bits = 96 octets) : deux vecteurs proches ont presque les
//! mêmes signes. Une recherche compare la question à tous les codes (des bits différents à
//! compter : quelques millisecondes pour un million de vecteurs), puis départage les meilleurs
//! candidats avec leurs vrais vecteurs, relus dans la base.

/// Un texte de moins de mots que ça (un bouton, un nom, une date) n'a pas de vecteur de sens : la
/// recherche par les mots le trouve, et le sens d'un ou deux mots ne vaut pas sa place (42 % des
/// textes vus en novembre 2026, qui remontaient en tête de la recherche par le sens sans rien dire).
pub const MOTS_MIN: usize = 3;

/// Ce texte mérite-t-il un vecteur de sens ? (au moins [`MOTS_MIN`] mots)
pub fn a_comprendre(texte: &str) -> bool {
    texte.split_whitespace().nth(MOTS_MIN - 1).is_some()
}

/// Les bits de signe d'un vecteur (1 si la valeur est positive), 64 par mot.
pub fn code<T: Copy + Into<f32>>(vecteur: &[T]) -> Vec<u64> {
    let mut mots = vec![0u64; vecteur.len().div_ceil(64)];
    for (i, &x) in vecteur.iter().enumerate() {
        if x.into() > 0.0 {
            mots[i / 64] |= 1 << (i % 64);
        }
    }
    mots
}

/// Le code d'un vecteur en octets signés.
pub fn code_octets(vecteur: &[i8]) -> Vec<u64> {
    let mut mots = vec![0u64; vecteur.len().div_ceil(64)];
    for (i, &x) in vecteur.iter().enumerate() {
        if x > 0 {
            mots[i / 64] |= 1 << (i % 64);
        }
    }
    mots
}

/// Un code en octets (pour la base) et retour.
pub fn en_octets(code: &[u64]) -> Vec<u8> {
    code.iter().flat_map(|m| m.to_le_bytes()).collect()
}

pub fn depuis_octets(octets: &[u8]) -> Vec<u64> {
    octets
        .chunks_exact(8)
        .map(|c| u64::from_le_bytes(c.try_into().unwrap_or([0; 8])))
        .collect()
}

/// Des codes de même longueur et ce qu'ils désignent (l'identifiant du vecteur dans la base).
#[derive(Debug, Default, Clone)]
pub struct IndexCompact {
    /// Mots de 64 bits par code (0 tant qu'aucun code n'est rangé).
    mots: usize,
    ids: Vec<i64>,
    codes: Vec<u64>,
}

impl IndexCompact {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Le dernier identifiant rangé (les vecteurs arrivent dans l'ordre de la base).
    pub fn dernier(&self) -> Option<i64> {
        self.ids.last().copied()
    }

    /// Range un code ; un code d'une autre longueur que les premiers est ignoré.
    pub fn ajouter(&mut self, id: i64, code: &[u64]) {
        if self.mots == 0 {
            self.mots = code.len();
        }
        if code.len() != self.mots || code.is_empty() {
            return;
        }
        self.ids.push(id);
        self.codes.extend_from_slice(code);
    }

    /// Place prise en mémoire (octets).
    pub fn octets(&self) -> usize {
        self.ids.capacity() * 8 + self.codes.capacity() * 8
    }

    /// Les `k` identifiants dont le code diffère le moins de celui de la question (dans le
    /// désordre).
    pub fn candidats(&self, question: &[u64], k: usize) -> Vec<i64> {
        if self.mots == 0 || question.len() != self.mots || k == 0 {
            return Vec::new();
        }
        let mut distances: Vec<(u32, u32)> = self
            .codes
            .chunks_exact(self.mots)
            .enumerate()
            .map(|(i, c)| (c.iter().zip(question).map(|(a, b)| (a ^ b).count_ones()).sum::<u32>(), i as u32))
            .collect();
        if distances.len() > k {
            distances.select_nth_unstable(k - 1);
            distances.truncate(k);
        }
        distances.into_iter().map(|(_, i)| self.ids[i as usize]).collect()
    }
}

impl IndexCompact {
    /// Les `k` identifiants dont le code s'accorde le mieux avec la question gardée en vrais
    /// nombres : le score d'un code est la somme des nombres de la question, chacun avec le signe
    /// du code (plus fidèle que des bits contre des bits quand question et vecteurs ne se
    /// ressemblent pas, comme un texte et des images). Une table par octet du code (256 sommes
    /// d'avance) : 96 additions par vecteur de 768 nombres.
    pub fn candidats_fins(&self, question: &[f32], k: usize) -> Vec<i64> {
        let octets_par_code = self.mots * 8;
        if self.mots == 0 || question.len() != octets_par_code * 8 || k == 0 {
            return Vec::new();
        }
        let mut table = vec![0f32; octets_par_code * 256];
        for j in 0..octets_par_code {
            let q = &question[j * 8..j * 8 + 8];
            for b in 0..256usize {
                table[j * 256 + b] = (0..8).map(|t| if b >> t & 1 == 1 { q[t] } else { -q[t] }).sum();
            }
        }
        let mut scores: Vec<(f32, u32)> = self
            .codes
            .chunks_exact(self.mots)
            .enumerate()
            .map(|(i, c)| {
                let mut s = 0f32;
                for (m, mot) in c.iter().enumerate() {
                    let base = m * 8 * 256;
                    for o in 0..8 {
                        s += table[base + o * 256 + ((mot >> (8 * o)) & 0xff) as usize];
                    }
                }
                (-s, i as u32)
            })
            .collect();
        if scores.len() > k {
            scores.select_nth_unstable_by(k - 1, |a, b| a.0.total_cmp(&b.0));
            scores.truncate(k);
        }
        scores.into_iter().map(|(_, i)| self.ids[i as usize]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_et_candidats() {
        let a: Vec<f32> = (0..768).map(|i| ((i as f32) * 0.37).sin()).collect();
        let presque_a: Vec<f32> = a.iter().enumerate().map(|(i, x)| if i % 50 == 0 { -x } else { *x }).collect();
        let oppose: Vec<f32> = a.iter().map(|x| -x).collect();
        let octets: Vec<i8> = a.iter().map(|x| (x * 100.0) as i8).collect();
        assert_eq!(code(&a).len(), 12);
        // Même signe en flottants et en octets (hors zéros).
        let ca = code(&a);
        let co = code_octets(&octets);
        assert!(ca.iter().zip(&co).map(|(x, y)| (x ^ y).count_ones()).sum::<u32>() < 10);
        assert_eq!(depuis_octets(&en_octets(&ca)), ca);
        let mut index = IndexCompact::default();
        index.ajouter(1, &code(&oppose));
        index.ajouter(2, &code(&presque_a));
        index.ajouter(3, &code(&a));
        index.ajouter(4, &[1, 2]); // pas la bonne longueur
        assert_eq!(index.len(), 3);
        let mut deux = index.candidats(&ca, 2);
        deux.sort();
        assert_eq!(deux, vec![2, 3]);
        assert_eq!(index.candidats(&ca, 10).len(), 3);
        assert_eq!(index.dernier(), Some(3));
        let mut fins = index.candidats_fins(&a, 2);
        fins.sort();
        assert_eq!(fins, vec![2, 3]);
    }
}
