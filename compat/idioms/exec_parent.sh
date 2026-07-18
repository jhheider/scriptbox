here=$(CDPATH= cd -- "$(dirname -- "${SCRIPTBOX_SOURCE:-$0}")" && pwd)
bash "$here/exec_child.sh"
printf 'parent-done\n'
