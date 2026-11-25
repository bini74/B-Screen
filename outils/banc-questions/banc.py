"""Banc d'essai « comprendre une question » (phase 7/8) : quel modèle LOCAL pourrait remplacer Claude
pour transformer une question en plan de recherche ? Mesure la durée et la justesse du plan.
Utilisation : python3 -I banc.py [modèle Ollama ...]   (par défaut : les modèles installés listés ci-dessous)"""
import json, sys, time, urllib.request

CONSIGNE = """Tu transformes une question sur la mémoire de l'écran et de la voix de Julien en plan de recherche.
Réponds UNIQUEMENT en JSON : {"mots": [mots ou synonymes à chercher], "personnes": [prénoms], "periode": "aujourd'hui|hier|lundi|…|null", "sources": ["ecran","voix","recherches"]}.
Élargis les mots (synonymes, exemples concrets) ; « je cherchais » → source "recherches"."""

# question → ce que le plan doit contenir (au moins un mot attendu, personnes, période, source)
QUESTIONS = [
    ("mollusque", {"mots": ["escargot", "huître", "moule", "coquillage"], "sources": ["ecran"]}),
    ("on a parlé de quoi hier avec Camille", {"personnes": ["Camille"], "periode": "hier", "sources": ["voix"]}),
    ("le poisson que je cherchais", {"mots": ["poisson", "aquarium"], "sources": ["recherches"]}),
    ("la vidéo drôle sur le judoka", {"mots": ["Riner", "judo", "humour", "humoriste"], "sources": ["ecran"]}),
    ("qu'est-ce que la dame de la caisse m'a dit ce matin", {"mots": ["caisse", "encaissement", "TPE"], "periode": "aujourd'hui", "sources": ["voix"]}),
    ("le dessin animé avec des espionnes", {"mots": ["Totally Spies", "espionnes", "dessin animé"], "sources": ["ecran"]}),
    ("l'actrice que j'ai cherchée sur Google", {"mots": ["actrice"], "sources": ["recherches"]}),
    ("le message d'erreur de compilation de lundi", {"mots": ["erreur", "error", "compilation", "build"], "periode": "lundi", "sources": ["ecran"]}),
]

def demander(modele, question):
    corps = {"model": modele, "stream": False, "format": "json", "think": False,
             "options": {"temperature": 0},
             "messages": [{"role": "system", "content": CONSIGNE}, {"role": "user", "content": question}]}
    req = urllib.request.Request("http://localhost:11434/api/chat", data=json.dumps(corps).encode(),
                                 headers={"Content-Type": "application/json"})
    debut = time.time()
    r = json.load(urllib.request.urlopen(req, timeout=300))
    return time.time() - debut, r["message"]["content"]

def note(plan, attendu):
    points, total = 0, 0
    bas = lambda xs: [str(x).lower() for x in (xs or [])]
    if "mots" in attendu:
        total += 1; points += any(any(a.lower() in m for m in bas(plan.get("mots"))) for a in attendu["mots"])
    if "personnes" in attendu:
        total += 1; points += all(p.lower() in bas(plan.get("personnes")) for p in attendu["personnes"])
    if "periode" in attendu:
        total += 1; points += attendu["periode"] in str(plan.get("periode", "")).lower()
    if "sources" in attendu:
        total += 1; points += all(s in bas(plan.get("sources")) for s in attendu["sources"])
    return points, total

modeles = sys.argv[1:] or ["gemma4:12b", "qwen3.5:9b"]
for modele in modeles:
    demander(modele, "bonjour")  # chargement en mémoire, non compté
    durees, points, total, json_ok = [], 0, 0, 0
    for question, attendu in QUESTIONS:
        d, texte = demander(modele, question)
        durees.append(d)
        try:
            plan = json.loads(texte); json_ok += 1
        except json.JSONDecodeError:
            plan = {}
        p, t = note(plan, attendu); points += p; total += t
        if "-v" in sys.argv[0:1] or True:
            print(f"  {modele:<14} {d:4.1f} s  {p}/{t}  {question[:40]:<40} {texte[:110]}")
    durees.sort()
    print(f"{modele:<14} justesse {points}/{total} ({points/total:.0%})  JSON valide {json_ok}/{len(QUESTIONS)}  "
          f"médiane {durees[len(durees)//2]:.1f} s  pire {durees[-1]:.1f} s\n", flush=True)
