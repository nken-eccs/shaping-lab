#include "hb.h"
#include <cstddef>

static_assert(sizeof(hb_bool_t) == 4);
static_assert(sizeof(hb_direction_t) == 4);
static_assert(sizeof(hb_script_t) == 4);
static_assert(sizeof(hb_feature_t) == 16);
static_assert(sizeof(hb_variation_t) == 8);
static_assert(sizeof(hb_glyph_info_t) == 20);
static_assert(offsetof(hb_glyph_info_t, cluster) == 8);
static_assert(sizeof(hb_glyph_position_t) == 20);
static_assert(offsetof(hb_glyph_position_t, y_offset) == 12);
static_assert(offsetof(hb_segment_properties_t, language) == 8);
static_assert(sizeof(hb_segment_properties_t) == 8 + 3 * sizeof(void *));
static_assert(HB_MEMORY_MODE_READONLY == 1);
static_assert(HB_DIRECTION_LTR == 4 && HB_DIRECTION_RTL == 5);
static_assert(HB_DIRECTION_TTB == 6 && HB_DIRECTION_BTT == 7);
static_assert(HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES == 0);
static_assert(HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS == 1);
static_assert(HB_BUFFER_CLUSTER_LEVEL_CHARACTERS == 2);
static_assert(HB_BUFFER_CLUSTER_LEVEL_GRAPHEMES == 3);
static_assert(HB_GLYPH_FLAG_DEFINED == 7);

extern "C" int shaping_lab_harfbuzz_assertions_enabled() {
#ifdef NDEBUG
    return 0;
#else
    return 1;
#endif
}
