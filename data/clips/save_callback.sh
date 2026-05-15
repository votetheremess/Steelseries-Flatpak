#!/bin/sh
# Invoked by gpu-screen-recorder's -sc flag after a clip is saved.
# $1 is the absolute path of the saved file.
# We write it to a FIFO that the backend thread reads.
if [ -n "$ARCTIS_CHATMIX_SAVE_FIFO" ] && [ -p "$ARCTIS_CHATMIX_SAVE_FIFO" ]; then
    printf '%s\n' "$1" > "$ARCTIS_CHATMIX_SAVE_FIFO"
fi
