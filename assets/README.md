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
| `ne_10m_roads_major.geojson` | Major highways, beltways, and bypasses | 1:10m |

The roads layer is filtered from Natural Earth `ne_10m_roads` to the Major
Highway, Beltway, and Bypass feature types; the raw 49 MB collection is not
embedded. The files are embedded at compile time. No tile server, account, or
runtime network request is used for the map. Layers are filtered by map span so
a world view does not draw every settlement, state line, river, or highway.
