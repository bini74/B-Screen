#!/bin/bash
# Pousse B-Screen sur GitHub, dans un dépôt PRIVÉ (créé s'il n'existe pas).
#
#   scripts/pousser.sh                      demande le jeton (rien ne s'affiche en le collant)
#   GITHUB_TOKEN=… scripts/pousser.sh       ou le prend dans l'environnement
#
# Le jeton n'est écrit dans aucun fichier et ne passe jamais en argument d'une commande
# (invisible dans la liste des processus) : curl le lit sur son entrée, git dans son environnement.
set -euo pipefail
cd "$(dirname "$0")/.."

DEPOT="${BSCREEN_DEPOT:-bini74/b-screen}"
if [[ -z "${GITHUB_TOKEN:-}" ]]; then
	read -rsp "Jeton GitHub (droit « repo ») : " GITHUB_TOKEN
	echo
fi
[[ -n "$GITHUB_TOKEN" ]] || { echo "pas de jeton" >&2; exit 1; }

api() { # api MÉTHODE CHEMIN [CORPS] → code HTTP
	local corps="${3:-}"
	{
		printf 'header = "Authorization: Bearer %s"\n' "$GITHUB_TOKEN"
		printf 'header = "Accept: application/vnd.github+json"\n'
		printf 'request = "%s"\n' "$1"
		printf 'url = "https://api.github.com%s"\n' "$2"
		[[ -n "$corps" ]] && printf 'data = "%s"\n' "${corps//\"/\\\"}"
	} | curl -s -o /dev/null -w '%{http_code}' --config -
}

case "$(api GET "/repos/$DEPOT")" in
	200) echo "dépôt $DEPOT trouvé" ;;
	404)
		nom="${DEPOT#*/}"
		code=$(api POST /user/repos "{\"name\":\"$nom\",\"private\":true,\"description\":\"B-Screen : la mémoire de l'écran pour macOS, en Rust\"}")
		[[ "$code" == 201 ]] || { echo "création du dépôt refusée (HTTP $code)" >&2; exit 1; }
		echo "dépôt privé $DEPOT créé"
		;;
	401) echo "jeton refusé par GitHub" >&2; exit 1 ;;
	*) echo "GitHub ne répond pas comme prévu" >&2; exit 1 ;;
esac

# Le jeton passe par l'environnement de git (GIT_CONFIG_*), jamais en argument.
ENTETE="Authorization: Basic $(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64)"
GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=http.https://github.com/.extraheader GIT_CONFIG_VALUE_0="$ENTETE" \
	git push "https://github.com/$DEPOT.git" HEAD:main
unset GITHUB_TOKEN ENTETE
echo "poussé : https://github.com/$DEPOT (privé)"
