# Embedded basemap data

All files are Natural Earth data, which is in the public domain:

https://www.naturalearthdata.com/about/terms-of-use/

Source repository:

https://github.com/nvkelso/natural-earth-vector

| File | Layer | Scale |
|---|---|---|
| `ne_110m_admin_0_countries.geojson` | Admin 0 countries | 1:110m |
| `ne_10m_populated_places_simple.geojson` | Populated places | 1:10m |
| `ne_50m_admin_1_states_provinces_lines.geojson` | Admin 1 boundary lines | 1:50m |
| `ne_50m_rivers_lake_centerlines.geojson` | Rivers and lake centerlines | 1:50m |

The files are embedded at compile time. No tile server, account, or runtime
network request is used for the map. Layers are filtered by map span so a world
view does not draw every settlement, state line, or river.
