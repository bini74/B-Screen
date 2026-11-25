# Crédits

## Empreintes de voix (voix familières)

B-Screen utilise deux modèles Core ML, **modifiés** (convertis pour Core ML) :
`FBank.mlmodelc` et `Embedding.mlmodelc`, récupérés par `scripts/modeles.sh` depuis
[FluidInference/speaker-diarization-coreml](https://huggingface.co/FluidInference/speaker-diarization-coreml).

- Chaîne d'origine : [pyannote/speaker-diarization-community-1](https://huggingface.co/pyannote/speaker-diarization-community-1) (pyannote), modèle d'empreintes WeSpeaker.
- Conversion Core ML (formes fixes, précision mixte, séparation FBank / empreinte) : Fluid Inference.
- Licence : [Creative Commons Attribution 4.0 (CC-BY-4.0)](https://creativecommons.org/licenses/by/4.0/).

Références citées par les auteurs :

- Hervé Bredin, *pyannote.audio 2.1 speaker diarization pipeline: principle, benchmark, and recipe*, Interspeech 2023.
- Hongji Wang et al., *WeSpeaker: A Research and Production oriented Speaker Embedding Learning Toolkit*, ICASSP 2023.

## Recherche par le sens

`EmbeddingGemma.mlmodelc` : [google/embeddinggemma-300m](https://huggingface.co/google/embeddinggemma-300m)
(Google), **modifié** (converti pour Core ML en précision mixte par `outils/coreml/convertir.py`).
Licence : [Gemma Terms of Use](https://ai.google.dev/gemma/terms).

## Recherche dans les images

`SigLIP2-Image.mlmodelc` et `SigLIP2-Texte.mlmodelc` : [google/siglip2-base-patch16-256](https://huggingface.co/google/siglip2-base-patch16-256)
(Google), **modifiés** : convertis pour Core ML par `outils/coreml/convertir_siglip.py` (image en
16 bits ; texte quantifié en 8 bits), avec leur tokeniseur (`SigLIP2.tokenizer.json`).
Licence : [Apache 2.0](https://www.apache.org/licenses/LICENSE-2.0) (texte dans `assets/licences/Apache-2.0.txt`,
joint à l'app).

- Michael Tschannen et al., *SigLIP 2: Multilingual Vision-Language Encoders with Improved Semantic
  Understanding, Localization, and Dense Features*, 2025.

MobileCLIP 2 (Apple), prévu d'abord, n'a pas été retenu : ses poids ne sont permis que pour la
recherche scientifique (« Apple Machine Learning Research Model License »).

## Photos du banc d'essai

Dans `crates/bscreen/banc/photos/`, réduites, trouvées par [Openverse](https://openverse.org) :

| Fichier | Photo | Auteur | Licence |
|---|---|---|---|
| `voiture-rouge.jpg` | [Little red car](https://www.flickr.com/photos/135584606@N08/22453998786) | estepa.andrea | CC0 |
| `voiture-bleue.jpg` | [Blue Car](https://www.flickr.com/photos/45379817@N08/5241438057) | blizzy63 | domaine public |
| `velo-rouge.jpg` | [The Red Bicycle.](https://www.flickr.com/photos/88123769@N02/15655615295) | Bernard Spragg | CC0 |
| `pompiers.jpg` | [169th Civil Engineer Squadron fire fighters re-certify a fire truck](https://www.flickr.com/photos/85045231@N04/52249764925) | S.C. Air National Guard | domaine public |

## Briques d'Apple

ScreenCaptureKit, AVFoundation, Vision (OCR), Speech (transcription), SoundAnalysis (parole ou musique)
et Core ML, appelés depuis Rust par les liaisons [objc2](https://github.com/madsmtm/objc2).
