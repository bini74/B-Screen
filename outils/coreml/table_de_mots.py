"""La table des mots d'un modèle de texte, sortie du modèle Core ML (24/11/2026).

Les deux modèles de texte de B-Screen (EmbeddingGemma pour le sens, SigLIP 2 pour les images)
commencent par une table de plus de 256 000 mots × 768 nombres : les deux tiers de leur poids.
Dans Core ML, elle était chargée en entier (≈ 400 Mo en 16 bits), et macOS la recopiait dans son
cache à chaque recompilation. B-Screen la lit maintenant lui-même, en mémoire projetée : seules
les lignes des mots d'une phrase sont lues sur le disque.

Format du fichier `.mots` (petit-boutiste) :
    "BSMOTS01"            8 octets
    lignes                u32
    dimension             u32
    échelles              f32 × lignes
    valeurs               i8 × lignes × dimension     (valeur ≈ octet × échelle de sa ligne)
"""
import numpy as np

MAGIE = b"BSMOTS01"


def quantifier(table):
    """Octets signés, une échelle par ligne (le plus grand écart de la ligne vaut 127)."""
    table = np.asarray(table, np.float32)
    maximum = np.abs(table).max(axis=1)
    echelles = np.where(maximum > 0, maximum / 127.0, 1.0).astype(np.float32)
    octets = np.clip(np.rint(table / echelles[:, None]), -127, 127).astype(np.int8)
    return echelles, octets


def ecrire(chemin, table):
    echelles, octets = quantifier(table)
    lignes, dimension = octets.shape
    with open(chemin, "wb") as f:
        f.write(MAGIE)
        f.write(np.array([lignes, dimension], "<u4").tobytes())
        f.write(echelles.astype("<f4").tobytes())
        f.write(octets.tobytes())
    return echelles, octets


def relire(echelles, octets, ids):
    """Les lignes de ces mots, comme B-Screen les relit."""
    return octets[np.asarray(ids)].astype(np.float32) * echelles[np.asarray(ids)][:, None]
