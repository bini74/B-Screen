"""Convertit EmbeddingGemma (sentence-transformers) en Core ML : lignes de la table des mots -> vecteur normalisé (768).

Depuis le 24/11/2026, la table des 262 144 mots n'est plus dans le modèle Core ML (≈ 400 Mo en
16 bits, chargés en entier, recopiés par macOS dans son cache à chaque recompilation) : elle est
écrite à part dans EmbeddingGemma.mots (octets signés, une échelle par ligne, déjà multipliée par
la racine de 768 comme le fait Gemma ; format dans table_de_mots.py), et B-Screen la lit lui-même
en mémoire projetée. Le modèle prend `inputs_embeds` (1, 128, 768) et `attention_mask` (1, 128).

Utilisation : .venv/bin/python convertir.py   (modèle dans modele-hf/), puis
xcrun coremlcompiler compile EmbeddingGemma-128-fpmixte-fixe.mlpackage <dossier> et renommer en
EmbeddingGemma.mlmodelc ; copier EmbeddingGemma.mots à côté."""
import os, sys, numpy as np, torch, coremltools as ct
import table_de_mots
import coremltools.converters.mil.frontend.torch.ops as _ops

# coremltools refuse int() sur un tableau à 1 élément (tailles de têtes d'attention) : on prend l'élément.
_cast_orig = _ops._cast
def _cast(context, node, dtype, dtype_name):
    x = context[node.inputs[0]]
    if x.val is not None and getattr(x.val, "ndim", 0) > 0 and x.val.size == 1:
        context.add(_ops.mb.const(val=dtype(np.asarray(x.val).ravel()[0]), name=node.name))
        return
    return _cast_orig(context, node, dtype, dtype_name)
_ops._cast = _cast
PRECISION = os.environ.get("PRECISION", "mixte")
# Une seule longueur fixe (128 jetons) : avec plusieurs longueurs (EnumeratedShapes) et une entrée
# en flottants, la carte graphique tombait à 116 ms par texte au lieu de 17 (24/11/2026). FIXE=0 : 64, 128 et 256.
FIXE = os.environ.get("FIXE", "1") == "1"
LONGUEURS = [64, 128, 256]
from sentence_transformers import SentenceTransformer

LONGUEUR = int(sys.argv[1]) if len(sys.argv) > 1 else 128
st = SentenceTransformer("modele-hf", device="cpu", model_kwargs={"attn_implementation": "eager"})
st.eval()
transfo = st[0].auto_model
denses = [m for m in st if m.__class__.__name__ == "Dense"]
mots = transfo.embed_tokens
table = (mots.weight.detach().float() * mots.embed_scale.float()).numpy()
echelles, octets = table_de_mots.ecrire("EmbeddingGemma.mots", table)
print(f"table des mots : {table.shape[0]} × {table.shape[1]}, {octets.nbytes / 1e6:.0f} Mo en octets signés")

class Enveloppe(torch.nn.Module):
    def __init__(self):
        super().__init__()
        self.transfo = transfo
        self.denses = torch.nn.ModuleList(denses)
    def forward(self, inputs_embeds, attention_mask):
        # Masque fait main (attention bidirectionnelle, fenêtre glissante > LONGUEUR : les deux identiques).
        L = inputs_embeds.shape[1]
        bloque = (1.0 - attention_mask[:, None, None, :].to(torch.float32)) * -1e4
        masque = bloque.expand(-1, 1, L, L)
        positions = torch.arange(inputs_embeds.shape[1], dtype=torch.long).unsqueeze(0)
        h = self.transfo(inputs_embeds=inputs_embeds, position_ids=positions, cache_position=positions[0], use_cache=False,
                         attention_mask={"full_attention": masque, "sliding_attention": masque}).last_hidden_state
        m = attention_mask.unsqueeze(-1).to(h.dtype)
        v = (h * m).sum(1) / m.sum(1).clamp(min=1.0)
        for d in self.denses:
            v = d({"sentence_embedding": v})["sentence_embedding"]
        return torch.nn.functional.normalize(v, dim=-1)

env = Enveloppe().eval()
tok = st.tokenizer
ex = tok(["title: none | text: bonjour"], padding="max_length", max_length=LONGUEUR, truncation=True, return_tensors="pt")
lignes_ex = torch.from_numpy(table_de_mots.relire(echelles, octets, ex["input_ids"][0]))[None]
with torch.no_grad():
    trace = torch.jit.trace(env, (lignes_ex, ex["attention_mask"]), strict=False)
modele = ct.convert(
    trace,
    # Plusieurs longueurs : les textes courts (la plupart) passent en 64 jetons, bien plus vite.
    inputs=([ct.TensorType(name="inputs_embeds", shape=(1, LONGUEUR, 768), dtype=np.float32),
             ct.TensorType(name="attention_mask", shape=(1, LONGUEUR), dtype=np.int32)] if FIXE else
            [ct.TensorType(name="inputs_embeds", shape=ct.EnumeratedShapes(shapes=[(1, n, 768) for n in LONGUEURS], default=(1, LONGUEUR, 768)), dtype=np.float32),
             ct.TensorType(name="attention_mask", shape=ct.EnumeratedShapes(shapes=[(1, n) for n in LONGUEURS], default=(1, LONGUEUR)), dtype=np.int32)]),
    outputs=[ct.TensorType(name="vecteur")],
    minimum_deployment_target=ct.target.macOS15,
    compute_precision=(ct.precision.FLOAT32 if PRECISION == "32" else ct.precision.FLOAT16 if PRECISION == "16"
        # mixte : normalisations, softmax et grandes sommes en FP32, le reste en FP16
        else ct.transform.FP16ComputePrecision(op_selector=lambda op: op.op_type not in {
            "reduce_mean", "reduce_sum", "pow", "rsqrt", "sqrt", "real_div", "softmax", "square", "reduce_l2_norm"})),
    convert_to="mlprogram",
)
modele.save(f"EmbeddingGemma-{LONGUEUR}-fp{PRECISION}{'-fixe' if FIXE else ''}.mlpackage")

# Vérification : mêmes vecteurs que sentence-transformers ?
textes = ["title: none | text: La vente du Pendant s'arrête et l'app Rewind est abandonnée.",
          "task: search result | query: l'entreprise arrête son collier enregistreur",
          "title: none | text: Des Tatoueurs jugent les tatouages de Rappeurs",
          "task: search result | query: le chargeur du mac",
          "title: none | text: fn main() { let base = Base::ouvrir(&chemin)?; println!(\"{}\", base.compter()); }",
          "title: none | text: Facture Free Mobile novembre 2026 : 19,99 € prélevés le 05/12",
          "task: search result | query: quand ai-je parlé de la voiture avec Camille ?",
          "title: none | text: " + " ".join(["Le compte rendu de la réunion de lundi avec l'équipe : budget, planning, recrutement."] * 6)]
ref = st.encode(textes, normalize_embeddings=True)
for t, r in zip(textes, ref):
    e = tok([t], truncation=True, max_length=256, return_tensors="np")
    n = LONGUEUR if FIXE else next(l for l in LONGUEURS if l >= e["input_ids"].shape[1])
    e = tok([t], padding="max_length", max_length=n, truncation=True, return_tensors="np")
    lignes = table_de_mots.relire(echelles, octets, e["input_ids"][0])[None]
    v = modele.predict({"inputs_embeds": lignes, "attention_mask": e["attention_mask"].astype(np.int32)})["vecteur"][0]
    with torch.no_grad():
        seule = env(torch.from_numpy(lignes), torch.from_numpy(e["attention_mask"])).numpy()[0]
    print(f"{n:3d} jetons : cosinus Core ML / référence {float(np.dot(v, r) / np.linalg.norm(v)):.5f} (table seule {float(np.dot(seule, r)):.5f})")
