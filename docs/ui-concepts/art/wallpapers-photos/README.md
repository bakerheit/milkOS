# Photo wallpapers (candidates)

Six photographs for the wallpaper chooser, as shot (no colour change), 1024x768, JPEG quality 92 with no EXIF. Every one is **CC0 1.0** (public domain dedication), so they can live in this MIT repository; no credit is required, the authors are listed because they deserve it.

They are deliberately **not** in `../wallpapers/`: `scripts/art_embed.py` bakes every PNG in that folder into the kernel as raw RGB (about 2.3 MB each, photos do not run-length code), which would add about 14 MB to the kernel for six photos. They are meant to be loaded when chosen (see ART.md, "Photo wallpapers").

| File | Source | Author | What was done |
|---|---|---|---|
| `belfast-sunset` | [Poly Haven: Belfast Sunset](https://polyhaven.com/a/belfast_sunset) | Greg Zaal (processing), Dimitrios Savva (photography) | Flat 75 degree view out of the 360 degree panorama, facing the sunset, 8 degrees up. |
| `bell-park-dawn` | [Poly Haven: Bell Park Dawn](https://polyhaven.com/a/bell_park_dawn) | Greg Zaal | Flat 75 degree view out of the panorama, looking away from the sun across the lake. |
| `aarfontein-sunrise` | [Poly Haven: Aarfontein Sunrise](https://polyhaven.com/a/aarfontein_sunrise) | Dario Barresi | Flat 75 degree view out of the panorama, facing the sunrise. |
| `misty-dawn-field` | [WordPress Photo Directory](https://wordpress.org/photos/photo/13165dcb4a/) | bdthemes | Centre crop to 4:3 and resize. |
| `dusk-pasture` | [WordPress Photo Directory](https://wordpress.org/photos/photo/63267dce29/) | Shariful Islam | Centre crop to 4:3 and resize. |
| `cows-and-trees` | [WordPress Photo Directory](https://wordpress.org/photos/photo/296a697b04/) | Josthin Medina Daniels | Centre crop to 4:3 and resize. |

Poly Haven's Belfast Sunset and Bell Park Dawn panoramas were taken from its 8k tonemapped JPG; the licence terms are at <https://polyhaven.com/license>. The WordPress photos are CC0 by the Photo Directory's rules (<https://wordpress.org/photos/license/>); the original files carried GPS and camera data, which is not in these copies.

## Contrast
As shot, these fail the glass rule at the usual 55% glass (4/8 to 1/8 regions pass). With the glass at **75%** milk (dark theme: 75% dark milk) all six pass all eight regions:

    python3 docs/ui-concepts/art/check-wallpaper.py docs/ui-concepts/art/wallpapers-photos/dusk-pasture-1024x768.jpg --alpha 0.75
