#!/bin/sh
if [ "$1" = check-profile ]; then [ "$#" = 3 ] && [ "$2" = --service ] && [ -d "$3" ]; exit $?; fi
printf "%s\n" "$@"
cat
exit 7
