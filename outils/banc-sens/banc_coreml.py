"""Banc d'essai d'EmbeddingGemma converti pour Core ML (mêmes questions que banc.py)."""
import json, os, sqlite3, sys, time
import numpy as np, coremltools as ct
from transformers import AutoTokenizer

ICI = os.path.dirname(os.path.abspath(__file__))
PAQUET = sys.argv[1]
UNITES = {"tout": ct.ComputeUnit.ALL, "ane": ct.ComputeUnit.CPU_AND_NE, "gpu": ct.ComputeUnit.CPU_AND_GPU}[sys.argv[2] if len(sys.argv) > 2 else "tout"]
BASE = os.path.expanduser("~/Library/Application Support/B-Screen/index.sqlite")
db = sqlite3.connect(f"file:{BASE}?mode=ro", uri=True)
docs = [(str(i), c) for i, c in db.execute("SELECT id, contenu FROM textes")]
docs += [("v%d" % i, t) for i, t in db.execute("SELECT id, texte FROM voix WHERE texte IS NOT NULL")]
questions = [(str(c), q) for c, q in json.load(open(os.path.join(ICI, "questions.json")))]
tok = AutoTokenizer.from_pretrained(os.path.join(ICI, "../coreml/modele-hf"))
t = time.time(); modele = ct.models.MLModel(PAQUET, compute_units=UNITES); print(f"chargement {time.time()-t:.1f} s")
L = modele.get_spec().description.input[0].type.multiArrayType.shape[1]

def vecteur(texte):
    e = tok([texte], padding="max_length", max_length=L, truncation=True, return_tensors="np")
    return modele.predict({"input_ids": e["input_ids"].astype(np.int32), "attention_mask": e["attention_mask"].astype(np.int32)})["vecteur"][0]

t = time.time()
D = np.stack([vecteur("title: none | text: " + d) for _, d in docs])
t_docs = time.time() - t
t = time.time()
Q = np.stack([vecteur("task: search result | query: " + q) for _, q in questions])
t_q = (time.time() - t) / len(questions)
ids = [i for i, _ in docs]
rangs = []
for (c, _), q in zip(questions, Q):
    ordre = [ids[i] for i in np.argsort(-(D @ q))]
    rangs.append(ordre.index(c) + 1)
n = len(rangs)
print(f"Core ML {os.path.basename(PAQUET)} ({sys.argv[2] if len(sys.argv) > 2 else 'tout'}) : 1er {sum(r==1 for r in rangs)/n:.0%}  top5 {sum(r<=5 for r in rangs)/n:.0%}  "
      f"top20 {sum(r<=20 for r in rangs)/n:.0%}  MRR {sum(1/r for r in rangs)/n:.2f}  "
      f"indexation {t_docs:.0f} s ({t_docs/len(docs)*1000:.0f} ms/texte), question {t_q*1000:.0f} ms")
