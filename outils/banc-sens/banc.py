"""Banc d'essai de la recherche par le sens sur les vraies données de B-Screen (lecture seule).
Compare : plein texte (FTS5 actuel), EmbeddingGemma, BGE-M3, et la fusion plein texte + vecteurs."""
import json, os, re, sqlite3, sys, time, urllib.request
import math

BASE = os.path.expanduser("~/Library/Application Support/B-Screen/index.sqlite")
ICI = os.path.dirname(os.path.abspath(__file__))
db = sqlite3.connect(f"file:{BASE}?mode=ro", uri=True)
docs = [(str(i), c) for i, c in db.execute("SELECT id, contenu FROM textes")]
docs += [("v%d" % i, t) for i, t in db.execute("SELECT id, texte FROM voix WHERE texte IS NOT NULL")]
ids = [d[0] for d in docs]
questions = [(str(c), q) for c, q in json.load(open(os.path.join(ICI, "questions.json")))]

def embed(modele, textes):
    req = urllib.request.Request("http://localhost:11434/api/embed",
        data=json.dumps({"model": modele, "input": textes, "truncate": True}).encode(),
        headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=600))["embeddings"]

def norme(v):
    n = math.sqrt(sum(x*x for x in v)) or 1.0
    return [x/n for x in v]

def classement_vecteurs(modele, prefixe_q, prefixe_d):
    t = time.time()
    dv = []
    for i in range(0, len(docs), 64):
        dv += [norme(v) for v in embed(modele, [prefixe_d + d[1][:2000] for d in docs[i:i+64]])]
    t_docs = time.time() - t
    t = time.time()
    qv = [norme(v) for v in embed(modele, [prefixe_q + q for _, q in questions])]
    t_q = (time.time() - t) / len(questions)
    res = []
    for v in qv:
        scores = [sum(a*b for a, b in zip(v, d)) for d in dv]
        res.append([ids[i] for i in sorted(range(len(ids)), key=lambda i: -scores[i])])
    return res, t_docs, t_q

def classement_fts():
    res = []
    for _, q in questions:
        mots = [m for m in re.findall(r"\w+", q.lower()) if len(m) > 2]
        requete = " OR ".join(f'"{m}"*' for m in mots)
        try:
            lignes = [str(r[0]) for r in db.execute(
                "SELECT rowid FROM textes_fts WHERE textes_fts MATCH ? ORDER BY bm25(textes_fts) LIMIT 200", (requete,))]
        except sqlite3.OperationalError:
            lignes = []
        # les voix : par mots en commun
        vs = sorted([(sum(m in t.lower() for m in mots), i) for i, t in docs if i.startswith("v")], reverse=True)
        lignes += [i for s, i in vs if s > 0]
        res.append(lignes)
    return res

def fusion(*listes, k=60):
    out = []
    for rangs in zip(*listes):
        s = {}
        for l in rangs:
            for r, i in enumerate(l[:200]):
                s[i] = s.get(i, 0) + 1 / (k + r + 1)
        out.append(sorted(s, key=lambda i: -s[i]))
    return out

def mesurer(nom, res, extra=""):
    rangs = [(l.index(c) + 1 if c in l else None) for (c, _), l in zip(questions, res)]
    n = len(rangs)
    a1 = sum(r == 1 for r in rangs) / n
    a5 = sum(r is not None and r <= 5 for r in rangs) / n
    a20 = sum(r is not None and r <= 20 for r in rangs) / n
    mrr = sum(1 / r for r in rangs if r) / n
    print(f"{nom:<28} 1er {a1:5.0%}  top5 {a5:5.0%}  top20 {a20:5.0%}  MRR {mrr:.2f}  {extra}")
    return rangs

print(f"{len(docs)} textes, {len(questions)} questions\n")
fts = classement_fts()
r_fts = mesurer("plein texte (actuel)", fts)
resultats = {}
for nom, modele, pq, pd in [
    ("EmbeddingGemma", "embeddinggemma", "task: search result | query: ", "title: none | text: "),
    ("BGE-M3", "bge-m3", "", ""),
]:
    res, t_docs, t_q = classement_vecteurs(modele, pq, pd)
    resultats[nom] = res
    mesurer(nom, res, f"indexation {t_docs:.0f} s ({t_docs/len(docs)*1000:.0f} ms/texte), question {t_q*1000:.0f} ms")
    mesurer(nom + " + plein texte", fusion(res, fts))
if "-v" in sys.argv:
    print()
    for (c, q), *rs in zip(questions, fts, *resultats.values()):
        print(f"{q[:60]:<60}", *[(l.index(c) + 1 if c in l else '-') for l in rs])
