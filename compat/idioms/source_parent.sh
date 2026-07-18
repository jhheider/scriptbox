here=$(CDPATH= cd -- "$(dirname -- "${SCRIPTBOX_SOURCE:-$0}")" && pwd)
. "$here/child.sh"
printf 'sourced:%s\n' "$CHILDVAR"
