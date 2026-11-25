#!/bin/bash
# Récupère les modèles d'empreintes de voix (Core ML) dans assets/modeles/, une seule fois.
#
# Source : FluidInference/speaker-diarization-coreml (Hugging Face), artefacts « Community-1 »
# de pyannote (FBank + Embedding WeSpeaker), licence CC-BY-4.0 — voir CREDITS.md.
# Chaque fichier est vérifié par son empreinte SHA-256 : un fichier modifié est refusé.
set -euo pipefail
cd "$(dirname "$0")/.."

SOURCE="https://huggingface.co/FluidInference/speaker-diarization-coreml/resolve/main"
DOSSIER=assets/modeles

# fichier                                   sha256
ATTENDUS="
FBank.mlmodelc/weights/weight.bin            9e83fdd3ea78064b078069e4d9141603c61c47a27fd19e7e3142ff7476f8db36
FBank.mlmodelc/coremldata.bin                57ac436bb0671cbb5527a339134d695f752eb77f7a18966b93c6835335595759
FBank.mlmodelc/model.mil                     27aaeb21569e81bdbe2eef87789f50a37cfea800039bd134448a9417de2f30ed
Embedding.mlmodelc/weights/weight.bin        99356b2985b8d43880a657024d941d450b38820451ccff903f76ed4e52d1868b
Embedding.mlmodelc/coremldata.bin            4a705bac27d151d9642f37609296042a15602a42253039e0921dc9e75da7e004
Embedding.mlmodelc/model.mil                 22fa958aef72a561c21f874a07cbdcd30fdf40ee961c0bc2fb67c119273b46d3
"
# Fichiers d'accompagnement (sans effet sur les calculs) : pas d'empreinte fixée.
ANNEXES="FBank.mlmodelc/metadata.json FBank.mlmodelc/analytics/coremldata.bin Embedding.mlmodelc/metadata.json Embedding.mlmodelc/analytics/coremldata.bin"

telecharger() {
	mkdir -p "$DOSSIER/$(dirname "$1")"
	curl -fsSL --retry 2 --max-time 300 -o "$DOSSIER/$1.part" "$SOURCE/$1"
	mv "$DOSSIER/$1.part" "$DOSSIER/$1"
}

while read -r fichier empreinte; do
	[[ -z "$fichier" ]] && continue
	if [[ ! -f "$DOSSIER/$fichier" ]] || [[ "$(shasum -a 256 "$DOSSIER/$fichier" | cut -d' ' -f1)" != "$empreinte" ]]; then
		echo "téléchargement de $fichier"
		telecharger "$fichier"
	fi
	reelle=$(shasum -a 256 "$DOSSIER/$fichier" | cut -d' ' -f1)
	if [[ "$reelle" != "$empreinte" ]]; then
		echo "empreinte inattendue pour $fichier : $reelle" >&2
		rm -f "$DOSSIER/$fichier"
		exit 1
	fi
done <<< "$ATTENDUS"
for fichier in $ANNEXES; do
	[[ -f "$DOSSIER/$fichier" ]] || telecharger "$fichier"
done
echo "modèles prêts dans $DOSSIER"
