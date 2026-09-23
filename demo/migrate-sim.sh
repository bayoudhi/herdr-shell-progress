#!/bin/sh
# A stand-in for a long job that reacts to a keypress, used by demo/lock.tape.
#
# It prints a ticking batch counter and aborts the moment it reads a key —
# which is the point: while keylock holds the session locked, the keys never
# arrive and the counter keeps going. After the unlock phrase, the next key
# lands and the job aborts, exactly as it would have all along without keylock.

batches=${1:-40}
i=0

printf 'migrating (any key aborts)\n'
while [ "$i" -lt "$batches" ]; do
  i=$((i + 1))
  printf '\rbatch %d/%d migrated' "$i" "$batches"
  if IFS= read -rsn1 -t 1 key 2>/dev/null; then
    printf '\n\033[31mgot a keypress: migration aborted at batch %d\033[0m\n' "$i"
    exit 1
  fi
done

printf '\n\033[32mmigration finished\033[0m\n'
