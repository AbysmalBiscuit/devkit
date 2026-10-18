# Reads a unified diff and prints "path<TAB>hunk<TAB>line" for every line on
# the new side that a review comment can anchor to: added and context lines.
# A multi-line comment must keep both ends in one hunk.
/^diff --git/ { p = ""; left = right = 0; next }
left == 0 && right == 0 {
  if (/^\+\+\+ /) { p = ($2 == "/dev/null") ? "" : substr($0, 7) }
  else if (/^@@ /) {
    split($3, nw, ","); split($2, od, ",")
    n = substr(nw[1], 2); right = (2 in nw) ? nw[2] : 1; left = (2 in od) ? od[2] : 1; h++
  }
  next
}
/^\\/ { next }
/^-/ { left--; next }
/^\+/ { if (p != "") print p "\t" h "\t" n; n++; right--; next }
{ if (p != "") print p "\t" h "\t" n; n++; left--; right-- }
