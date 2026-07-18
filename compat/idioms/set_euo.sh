set -eu
x=ok
printf 'seteuo:%s\n' "$x"
printf 'a\nb\n' | { grep b; } && printf 'pipe-ok\n'
