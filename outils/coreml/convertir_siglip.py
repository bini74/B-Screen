"""Convertit SigLIP 2 (google/siglip2-base-patch16-256, Apache 2.0) en deux modèles Core ML pour la
recherche dans les images (phase 7.3) :
  - SigLIP2-Image : pixels (1, 3, 256, 256) dans [-1, 1] (RGB, écran écrasé à 256 × 256) → vecteur (1, 768) normé ;
  - SigLIP2-Texte : inputs_embeds (1, 64, 768) (les lignes de la table des mots, voir plus bas) → vecteur (1, 768) normé.
    Les jetons : texte en minuscules, EOS = 1 à la fin, 0 ensuite jusqu'à 64.
  - SigLIP2-Texte.mots : la table des 256 000 mots (octets signés, une échelle par ligne ; format dans
    table_de_mots.py), lue par B-Screen en mémoire projetée. Depuis le 24/11/2026 elle n'est plus
    dans le modèle Core ML : chargé, il pesait ≈ 565 Mo (les 8 bits sont remis en 16 bits sur le
    processeur), et macOS en recopiait autant dans son cache à chaque recompilation.
Le reste du texte est quantifié en 8 bits ; l'image reste en 16 bits.
Contrôle : chaque modèle Core ML doit redonner les vecteurs de PyTorch (cosinus ≥ 0,99).

Utilisation : .venv/bin/python convertir_siglip.py [--texte]   (modèle dans modele-siglip2/, voir
`hf download google/siglip2-base-patch16-256 --local-dir modele-siglip2` ; --texte : seulement le texte)."""
import os, sys, time, numpy as np, torch, coremltools as ct
import table_de_mots
import coremltools.converters.mil.frontend.torch.ops as _ops

# coremltools refuse int() sur un tableau à 1 élément (tailles de têtes d'attention) : on prend
# l'élément (même correctif que convertir.py).
_cast_orig = _ops._cast
def _cast(context, node, dtype, dtype_name):
    x = context[node.inputs[0]]
    if x.val is not None and getattr(x.val, "ndim", 0) > 0 and x.val.size == 1:
        context.add(_ops.mb.const(val=dtype(np.asarray(x.val).ravel()[0]), name=node.name))
        return
    return _cast_orig(context, node, dtype, dtype_name)
_ops._cast = _cast
from coremltools.optimize.coreml import OpLinearQuantizerConfig, OptimizationConfig, linear_quantize_weights
from transformers import AutoModel, AutoProcessor

torch.set_grad_enabled(False)
modele = AutoModel.from_pretrained("modele-siglip2", attn_implementation="eager").eval()
proc = AutoProcessor.from_pretrained("modele-siglip2")
print(f"échelle {modele.logit_scale.exp().item():.4f}  biais {modele.logit_bias.item():.4f}")


class Image(torch.nn.Module):
    def __init__(self):
        super().__init__()
        self.vision = modele.vision_model

    def forward(self, pixels):
        v = self.vision(pixel_values=pixels).pooler_output
        return v / v.norm(dim=-1, keepdim=True)


class Texte(torch.nn.Module):
    """La référence : des jetons au vecteur, comme PyTorch."""
    def __init__(self):
        super().__init__()
        self.texte = modele.text_model

    def forward(self, input_ids):
        v = self.texte(input_ids=input_ids).pooler_output
        return v / v.norm(dim=-1, keepdim=True)


class TexteSansTable(torch.nn.Module):
    """Ce que fait SiglipTextTransformer.forward, à partir des lignes de la table des mots."""
    def __init__(self):
        super().__init__()
        self.texte = modele.text_model

    def forward(self, inputs_embeds):
        t = self.texte
        h = t.embeddings(inputs_embeds=inputs_embeds)  # + positions
        h = t.encoder(inputs_embeds=h).last_hidden_state  # pas de masque : SigLIP lit aussi le remplissage
        h = t.final_layer_norm(h)
        v = t.head(h[:, -1, :])  # le dernier jeton
        return v / v.norm(dim=-1, keepdim=True)


def cosinus(a, b):
    a, b = np.asarray(a, np.float32).ravel(), np.asarray(b, np.float32).ravel()
    return float(a @ b / np.linalg.norm(a) / np.linalg.norm(b))


# --- Image
TEXTE_SEUL = "--texte" in sys.argv
pixels = torch.rand(1, 3, 256, 256) * 2 - 1
if not TEXTE_SEUL:
    trace = torch.jit.trace(Image().eval(), pixels)
    image = ct.convert(
        trace,
        inputs=[ct.TensorType(name="pixels", shape=(1, 3, 256, 256), dtype=np.float32)],
        outputs=[ct.TensorType(name="vecteur", dtype=np.float32)],
        compute_precision=ct.precision.FLOAT16,
        minimum_deployment_target=ct.target.macOS14,
    )
    image.short_description = "SigLIP 2 (google/siglip2-base-patch16-256, Apache 2.0) : vecteur d'une image d'écran"
    image.save("SigLIP2-Image.mlpackage")
    attendu = Image()(pixels).numpy()
    for unites in (ct.ComputeUnit.CPU_AND_GPU, ct.ComputeUnit.ALL):
        m = ct.models.MLModel("SigLIP2-Image.mlpackage", compute_units=unites)
        sortie = m.predict({"pixels": pixels.numpy()})["vecteur"]
        t = time.time()
        for _ in range(10):
            m.predict({"pixels": pixels.numpy()})
        print(f"image {unites.name} : cosinus {cosinus(sortie, attendu):.4f}, {(time.time() - t) * 100:.1f} ms par image")

# --- Texte
table = modele.text_model.embeddings.token_embedding.weight.detach().numpy()
echelles, octets = table_de_mots.ecrire("SigLIP2-Texte.mots", table)
print(f"table des mots : {table.shape[0]} × {table.shape[1]}, {octets.nbytes / 1e6:.0f} Mo en octets signés")
exemple = torch.zeros(1, 64, 768)
trace = torch.jit.trace(TexteSansTable().eval(), exemple)
texte = ct.convert(
    trace,
    inputs=[ct.TensorType(name="inputs_embeds", shape=(1, 64, 768), dtype=np.float32)],
    outputs=[ct.TensorType(name="vecteur", dtype=np.float32)],
    compute_precision=ct.precision.FLOAT16,
    minimum_deployment_target=ct.target.macOS14,
)
if os.environ.get("TEXTE_8BITS", "0") == "1":
    texte = linear_quantize_weights(texte, OptimizationConfig(global_config=OpLinearQuantizerConfig(mode="linear_symmetric")))
texte.short_description = "SigLIP 2 (google/siglip2-base-patch16-256, Apache 2.0) : vecteur d'un texte (64 lignes de SigLIP2-Texte.mots)"
texte.save("SigLIP2-Texte.mlpackage")
m = ct.models.MLModel("SigLIP2-Texte.mlpackage", compute_units=ct.ComputeUnit.CPU_ONLY)
PHRASES = ["la voiture rouge", "un chat qui dort sur un canapé", "le camion de pompiers", "le code rust de b-screen",
           "a red car parked in front of the house", "la facture d'électricité de novembre", "camille au téléphone",
           "une capture d'écran du terminal avec des erreurs", "the eiffel tower at night", "un graphique des ventes"]
for phrase in PHRASES:
    ids = proc.tokenizer(phrase.lower(), padding="max_length", max_length=64)["input_ids"]
    attendu = Texte()(torch.tensor([ids], dtype=torch.int32)).numpy()
    # La table seule en octets (le modèle de PyTorch derrière) : ce que coûte la table.
    lignes = table_de_mots.relire(echelles, octets, ids)[None]
    seule = TexteSansTable()(torch.from_numpy(lignes)).numpy()
    t = time.time()
    sortie = m.predict({"inputs_embeds": lignes})["vecteur"]
    print(f"texte « {phrase} » : cosinus {cosinus(sortie, attendu):.4f} (table seule {cosinus(seule, attendu):.4f}), {(time.time() - t) * 1000:.0f} ms")
