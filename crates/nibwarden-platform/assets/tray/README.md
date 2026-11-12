Tray icons as raw 32×32 RGBA, rendered from `packaging/desktop/icons/nibwarden-{locked,unlocked}-symbolic.svg`
(filled black or white):

    sed 's/#2e3436/#000000/' nibwarden-locked-symbolic.svg > locked-black.svg
    magick -background none -density 576 locked-black.svg -resize 32x32 -depth 8 rgba:locked-black.rgba
