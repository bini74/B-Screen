#!/bin/bash
# Fabrique B-Screen.app, la signe et l'installe dans ~/Applications.
#
#   scripts/empaqueter.sh            version optimisée
#   scripts/empaqueter.sh debug      version de développement (compile plus vite)
#   scripts/empaqueter.sh … --lancer  relance l'app une fois installée
#
# La signature utilise le certificat « Apple Development » : sa signature reste la même
# d'une compilation à l'autre, donc macOS garde les autorisations (écran, micro, accessibilité).
set -euo pipefail
cd "$(dirname "$0")/.."

MODE=release
LANCER=0
for argument in "$@"; do
	case "$argument" in
		debug) MODE=debug ;;
		release) MODE=release ;;
		--lancer) LANCER=1 ;;
		*) echo "argument inconnu : $argument" >&2; exit 2 ;;
	esac
done

IDENTITE="${BSCREEN_IDENTITE:-Apple Development: Julien Bonzi (MVNVD9QQFD)}"
DESTINATION="${BSCREEN_DESTINATION:-$HOME/Applications}"
APP=target/B-Screen.app

if [[ "$MODE" == release ]]; then
	cargo build --release -p bscreen
	BINAIRE=target/release/bscreen
else
	cargo build -p bscreen
	BINAIRE=target/debug/bscreen
fi
[[ -f assets/AppIcon.icns && -f assets/barre-36.rgba ]] || cargo run -q -p icones

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)

/bin/rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINAIRE" "$APP/Contents/MacOS/bscreen"
sed -e "s/@VERSION@/$VERSION/" -e "s/@BUILD@/$BUILD/" assets/Info.plist > "$APP/Contents/Info.plist"
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
# Modèles d'empreintes de voix (Core ML), vérifiés par empreinte SHA-256.
scripts/modeles.sh >/dev/null
mkdir -p "$APP/Contents/Resources/modeles"
cp -R assets/modeles/FBank.mlmodelc assets/modeles/Embedding.mlmodelc "$APP/Contents/Resources/modeles/"
# Modèle de sens (phase 7) : fabriqué par outils/coreml/convertir.py, puis compilé (coremlcompiler).
if [[ -d assets/modeles/EmbeddingGemma.mlmodelc ]]; then
	cp -Rp assets/modeles/EmbeddingGemma.mlmodelc assets/modeles/EmbeddingGemma.mots assets/modeles/EmbeddingGemma.tokenizer.json "$APP/Contents/Resources/modeles/"
else
	echo "modèle de sens absent (outils/coreml) : la recherche par le sens sera éteinte" >&2
fi
# Recherche dans les images (phase 7.3) : SigLIP 2, fabriqué par outils/coreml/convertir_siglip.py,
# puis compilé (coremlcompiler) ; licence Apache 2.0 (voir CREDITS.md).
if [[ -d assets/modeles/SigLIP2-Image.mlmodelc && -d assets/modeles/SigLIP2-Texte.mlmodelc ]]; then
	cp -Rp assets/modeles/SigLIP2-Image.mlmodelc assets/modeles/SigLIP2-Texte.mlmodelc assets/modeles/SigLIP2-Texte.mots assets/modeles/SigLIP2.tokenizer.json \
		"$APP/Contents/Resources/modeles/"
	cp assets/licences/Apache-2.0.txt "$APP/Contents/Resources/modeles/SigLIP2.LICENSE.txt"
else
	echo "modèle d'images absent (outils/coreml) : la recherche dans les images sera éteinte" >&2
fi

codesign --force --options runtime --timestamp=none \
	--entitlements assets/B-Screen.entitlements \
	--sign "$IDENTITE" "$APP"
codesign --verify --strict "$APP"

# Remplace la version installée (on arrête d'abord celle qui tourne).
# Arrêt propre (SIGTERM) : B-Screen termine la vidéo en cours avant de quitter.
if pkill -f "B-Screen.app/Contents/MacOS/bscreen$" 2>/dev/null; then
	for _ in $(seq 1 30); do
		pgrep -f "B-Screen.app/Contents/MacOS/bscreen$" >/dev/null || break
		sleep 0.2
	done
fi
mkdir -p "$DESTINATION"
/bin/rm -rf "$DESTINATION/B-Screen.app"
ditto "$APP" "$DESTINATION/B-Screen.app"
echo "installée : $DESTINATION/B-Screen.app ($VERSION, build $BUILD, $MODE)"

if [[ "$LANCER" == 1 ]]; then
	# LaunchServices refuse parfois juste après le remplacement (-600) : on vérifie et on réessaie.
	for essai in 1 2 3; do
		open "$DESTINATION/B-Screen.app" 2>/dev/null || true
		for _ in $(seq 1 25); do
			pgrep -f "B-Screen.app/Contents/MacOS/bscreen$" >/dev/null && break 2
			sleep 0.2
		done
		echo "pas encore lancée (essai $essai), nouvel essai…" >&2
		sleep 1
	done
	pgrep -f "B-Screen.app/Contents/MacOS/bscreen$" >/dev/null || { echo "B-Screen ne démarre pas" >&2; exit 1; }
	echo "lancée"
fi
