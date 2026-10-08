# >>> envc initialize >>>
if command -v envc >/dev/null 2>&1; then
    envc() {
        case "${1:-}" in
            use|push|unuse|pop)
                eval "$(ENVC_WRAPPED=1 command envc "$@")"
                ;;
            *)
                command envc "$@"
                ;;
        esac
    }
    eval "$(command envc autoload)"
fi
# <<< envc initialize <<<
