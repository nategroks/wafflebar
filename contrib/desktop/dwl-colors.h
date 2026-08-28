/* dwl border colours to match themes/workbench.css. Paste into dwl's config.h (the natewm-asm
 * checkout owns that file; this is a snippet, not a drop-in header).
 *
 * dwl draws a solid border per window — there is no titlebar unless the titlebar patch is applied.
 * The bevel look therefore lives in the border width and colour: 2px, accent when focused, shadow
 * when not, which is what the Workbench screen does with its window borders.
 *
 * CDE palette instead: rootcolor #9296a8, bordercolor #6c7080, focuscolor #a83e7a. */

static const int borderpx = 2;                    /* 2px, matching the bar's bevel weight */
static const float rootcolor[]   = COLOR(0x8c8c8cff);  /* desktop backdrop = the theme's well */
static const float bordercolor[] = COLOR(0x262626ff);  /* unfocused: the bevel shadow */
static const float focuscolor[]  = COLOR(0x6688bbff);  /* focused: Workbench blue */
static const float urgentcolor[] = COLOR(0xdd4422ff);  /* urgent: the alert band */

/* If the titlebar patch is in use, give it the same band + font as the bar:
 *     static const char *titlebarfont = "Tengoku:size=9";
 *     titlebar height 20, text #ffffff on focuscolor, #000000 on bordercolor.
 * Verify the font binds before trusting it: `fc-match Tengoku` must answer Tengoku, not a
 * fallback (scripts/install-fonts.sh --verify does exactly this check). */
