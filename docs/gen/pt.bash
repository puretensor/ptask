_pt() {
    local i cur prev opts cmd
    COMPREPLY=()
    if [[ "${BASH_VERSINFO[0]}" -ge 4 ]]; then
        cur="$2"
    else
        cur="${COMP_WORDS[COMP_CWORD]}"
    fi
    prev="$3"
    cmd=""
    opts=""

    for i in "${COMP_WORDS[@]:0:COMP_CWORD}"
    do
        case "${cmd},${i}" in
            ",$1")
                cmd="pt"
                ;;
            pt,accountability)
                cmd="pt__subcmd__accountability"
                ;;
            pt,add)
                cmd="pt__subcmd__add"
                ;;
            pt,approval)
                cmd="pt__subcmd__approval"
                ;;
            pt,approve)
                cmd="pt__subcmd__approve"
                ;;
            pt,backfill)
                cmd="pt__subcmd__backfill"
                ;;
            pt,bot)
                cmd="pt__subcmd__bot"
                ;;
            pt,branch)
                cmd="pt__subcmd__branch"
                ;;
            pt,bulk)
                cmd="pt__subcmd__bulk"
                ;;
            pt,context)
                cmd="pt__subcmd__context"
                ;;
            pt,delegate)
                cmd="pt__subcmd__delegate"
                ;;
            pt,depend)
                cmd="pt__subcmd__depend"
                ;;
            pt,digest)
                cmd="pt__subcmd__digest"
                ;;
            pt,dismiss)
                cmd="pt__subcmd__dismiss"
                ;;
            pt,distill)
                cmd="pt__subcmd__distill"
                ;;
            pt,done)
                cmd="pt__subcmd__done"
                ;;
            pt,edit)
                cmd="pt__subcmd__edit"
                ;;
            pt,export)
                cmd="pt__subcmd__export"
                ;;
            pt,gen-completions)
                cmd="pt__subcmd__gen__subcmd__completions"
                ;;
            pt,gen-manpage)
                cmd="pt__subcmd__gen__subcmd__manpage"
                ;;
            pt,goal)
                cmd="pt__subcmd__goal"
                ;;
            pt,help)
                cmd="pt__subcmd__help"
                ;;
            pt,kind)
                cmd="pt__subcmd__kind"
                ;;
            pt,list)
                cmd="pt__subcmd__list"
                ;;
            pt,log)
                cmd="pt__subcmd__log"
                ;;
            pt,mcp)
                cmd="pt__subcmd__mcp"
                ;;
            pt,next)
                cmd="pt__subcmd__next"
                ;;
            pt,plan)
                cmd="pt__subcmd__plan"
                ;;
            pt,priority)
                cmd="pt__subcmd__priority"
                ;;
            pt,promote)
                cmd="pt__subcmd__promote"
                ;;
            pt,reap)
                cmd="pt__subcmd__reap"
                ;;
            pt,reject)
                cmd="pt__subcmd__reject"
                ;;
            pt,remote)
                cmd="pt__subcmd__remote"
                ;;
            pt,reopen)
                cmd="pt__subcmd__reopen"
                ;;
            pt,review)
                cmd="pt__subcmd__review"
                ;;
            pt,rm)
                cmd="pt__subcmd__rm"
                ;;
            pt,scoring)
                cmd="pt__subcmd__scoring"
                ;;
            pt,search)
                cmd="pt__subcmd__search"
                ;;
            pt,serve)
                cmd="pt__subcmd__serve"
                ;;
            pt,show)
                cmd="pt__subcmd__show"
                ;;
            pt,snooze)
                cmd="pt__subcmd__snooze"
                ;;
            pt,start)
                cmd="pt__subcmd__start"
                ;;
            pt,token)
                cmd="pt__subcmd__token"
                ;;
            pt,tui)
                cmd="pt__subcmd__tui"
                ;;
            pt,undo)
                cmd="pt__subcmd__undo"
                ;;
            pt,view)
                cmd="pt__subcmd__view"
                ;;
            pt,why)
                cmd="pt__subcmd__why"
                ;;
            pt__subcmd__accountability,help)
                cmd="pt__subcmd__accountability__subcmd__help"
                ;;
            pt__subcmd__accountability,run)
                cmd="pt__subcmd__accountability__subcmd__run"
                ;;
            pt__subcmd__accountability__subcmd__help,help)
                cmd="pt__subcmd__accountability__subcmd__help__subcmd__help"
                ;;
            pt__subcmd__accountability__subcmd__help,run)
                cmd="pt__subcmd__accountability__subcmd__help__subcmd__run"
                ;;
            pt__subcmd__approval,consume)
                cmd="pt__subcmd__approval__subcmd__consume"
                ;;
            pt__subcmd__approval,decide)
                cmd="pt__subcmd__approval__subcmd__decide"
                ;;
            pt__subcmd__approval,expire)
                cmd="pt__subcmd__approval__subcmd__expire"
                ;;
            pt__subcmd__approval,help)
                cmd="pt__subcmd__approval__subcmd__help"
                ;;
            pt__subcmd__approval,list)
                cmd="pt__subcmd__approval__subcmd__list"
                ;;
            pt__subcmd__approval,notify)
                cmd="pt__subcmd__approval__subcmd__notify"
                ;;
            pt__subcmd__approval,payload)
                cmd="pt__subcmd__approval__subcmd__payload"
                ;;
            pt__subcmd__approval,request)
                cmd="pt__subcmd__approval__subcmd__request"
                ;;
            pt__subcmd__approval,show)
                cmd="pt__subcmd__approval__subcmd__show"
                ;;
            pt__subcmd__approval,verify)
                cmd="pt__subcmd__approval__subcmd__verify"
                ;;
            pt__subcmd__approval,withdraw)
                cmd="pt__subcmd__approval__subcmd__withdraw"
                ;;
            pt__subcmd__approval__subcmd__help,consume)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__consume"
                ;;
            pt__subcmd__approval__subcmd__help,decide)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__decide"
                ;;
            pt__subcmd__approval__subcmd__help,expire)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__expire"
                ;;
            pt__subcmd__approval__subcmd__help,help)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__help"
                ;;
            pt__subcmd__approval__subcmd__help,list)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__list"
                ;;
            pt__subcmd__approval__subcmd__help,notify)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__notify"
                ;;
            pt__subcmd__approval__subcmd__help,payload)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__payload"
                ;;
            pt__subcmd__approval__subcmd__help,request)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__request"
                ;;
            pt__subcmd__approval__subcmd__help,show)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__show"
                ;;
            pt__subcmd__approval__subcmd__help,verify)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__verify"
                ;;
            pt__subcmd__approval__subcmd__help,withdraw)
                cmd="pt__subcmd__approval__subcmd__help__subcmd__withdraw"
                ;;
            pt__subcmd__goal,abandon)
                cmd="pt__subcmd__goal__subcmd__abandon"
                ;;
            pt__subcmd__goal,add)
                cmd="pt__subcmd__goal__subcmd__add"
                ;;
            pt__subcmd__goal,done)
                cmd="pt__subcmd__goal__subcmd__done"
                ;;
            pt__subcmd__goal,help)
                cmd="pt__subcmd__goal__subcmd__help"
                ;;
            pt__subcmd__goal,link)
                cmd="pt__subcmd__goal__subcmd__link"
                ;;
            pt__subcmd__goal,ls)
                cmd="pt__subcmd__goal__subcmd__ls"
                ;;
            pt__subcmd__goal,orphans)
                cmd="pt__subcmd__goal__subcmd__orphans"
                ;;
            pt__subcmd__goal,set-parent)
                cmd="pt__subcmd__goal__subcmd__set__subcmd__parent"
                ;;
            pt__subcmd__goal,show)
                cmd="pt__subcmd__goal__subcmd__show"
                ;;
            pt__subcmd__goal,unlink)
                cmd="pt__subcmd__goal__subcmd__unlink"
                ;;
            pt__subcmd__goal__subcmd__help,abandon)
                cmd="pt__subcmd__goal__subcmd__help__subcmd__abandon"
                ;;
            pt__subcmd__goal__subcmd__help,add)
                cmd="pt__subcmd__goal__subcmd__help__subcmd__add"
                ;;
            pt__subcmd__goal__subcmd__help,done)
                cmd="pt__subcmd__goal__subcmd__help__subcmd__done"
                ;;
            pt__subcmd__goal__subcmd__help,help)
                cmd="pt__subcmd__goal__subcmd__help__subcmd__help"
                ;;
            pt__subcmd__goal__subcmd__help,link)
                cmd="pt__subcmd__goal__subcmd__help__subcmd__link"
                ;;
            pt__subcmd__goal__subcmd__help,ls)
                cmd="pt__subcmd__goal__subcmd__help__subcmd__ls"
                ;;
            pt__subcmd__goal__subcmd__help,orphans)
                cmd="pt__subcmd__goal__subcmd__help__subcmd__orphans"
                ;;
            pt__subcmd__goal__subcmd__help,set-parent)
                cmd="pt__subcmd__goal__subcmd__help__subcmd__set__subcmd__parent"
                ;;
            pt__subcmd__goal__subcmd__help,show)
                cmd="pt__subcmd__goal__subcmd__help__subcmd__show"
                ;;
            pt__subcmd__goal__subcmd__help,unlink)
                cmd="pt__subcmd__goal__subcmd__help__subcmd__unlink"
                ;;
            pt__subcmd__help,accountability)
                cmd="pt__subcmd__help__subcmd__accountability"
                ;;
            pt__subcmd__help,add)
                cmd="pt__subcmd__help__subcmd__add"
                ;;
            pt__subcmd__help,approval)
                cmd="pt__subcmd__help__subcmd__approval"
                ;;
            pt__subcmd__help,approve)
                cmd="pt__subcmd__help__subcmd__approve"
                ;;
            pt__subcmd__help,backfill)
                cmd="pt__subcmd__help__subcmd__backfill"
                ;;
            pt__subcmd__help,bot)
                cmd="pt__subcmd__help__subcmd__bot"
                ;;
            pt__subcmd__help,branch)
                cmd="pt__subcmd__help__subcmd__branch"
                ;;
            pt__subcmd__help,bulk)
                cmd="pt__subcmd__help__subcmd__bulk"
                ;;
            pt__subcmd__help,context)
                cmd="pt__subcmd__help__subcmd__context"
                ;;
            pt__subcmd__help,delegate)
                cmd="pt__subcmd__help__subcmd__delegate"
                ;;
            pt__subcmd__help,depend)
                cmd="pt__subcmd__help__subcmd__depend"
                ;;
            pt__subcmd__help,digest)
                cmd="pt__subcmd__help__subcmd__digest"
                ;;
            pt__subcmd__help,dismiss)
                cmd="pt__subcmd__help__subcmd__dismiss"
                ;;
            pt__subcmd__help,distill)
                cmd="pt__subcmd__help__subcmd__distill"
                ;;
            pt__subcmd__help,done)
                cmd="pt__subcmd__help__subcmd__done"
                ;;
            pt__subcmd__help,edit)
                cmd="pt__subcmd__help__subcmd__edit"
                ;;
            pt__subcmd__help,export)
                cmd="pt__subcmd__help__subcmd__export"
                ;;
            pt__subcmd__help,gen-completions)
                cmd="pt__subcmd__help__subcmd__gen__subcmd__completions"
                ;;
            pt__subcmd__help,gen-manpage)
                cmd="pt__subcmd__help__subcmd__gen__subcmd__manpage"
                ;;
            pt__subcmd__help,goal)
                cmd="pt__subcmd__help__subcmd__goal"
                ;;
            pt__subcmd__help,help)
                cmd="pt__subcmd__help__subcmd__help"
                ;;
            pt__subcmd__help,kind)
                cmd="pt__subcmd__help__subcmd__kind"
                ;;
            pt__subcmd__help,list)
                cmd="pt__subcmd__help__subcmd__list"
                ;;
            pt__subcmd__help,log)
                cmd="pt__subcmd__help__subcmd__log"
                ;;
            pt__subcmd__help,mcp)
                cmd="pt__subcmd__help__subcmd__mcp"
                ;;
            pt__subcmd__help,next)
                cmd="pt__subcmd__help__subcmd__next"
                ;;
            pt__subcmd__help,plan)
                cmd="pt__subcmd__help__subcmd__plan"
                ;;
            pt__subcmd__help,priority)
                cmd="pt__subcmd__help__subcmd__priority"
                ;;
            pt__subcmd__help,promote)
                cmd="pt__subcmd__help__subcmd__promote"
                ;;
            pt__subcmd__help,reap)
                cmd="pt__subcmd__help__subcmd__reap"
                ;;
            pt__subcmd__help,reject)
                cmd="pt__subcmd__help__subcmd__reject"
                ;;
            pt__subcmd__help,remote)
                cmd="pt__subcmd__help__subcmd__remote"
                ;;
            pt__subcmd__help,reopen)
                cmd="pt__subcmd__help__subcmd__reopen"
                ;;
            pt__subcmd__help,review)
                cmd="pt__subcmd__help__subcmd__review"
                ;;
            pt__subcmd__help,rm)
                cmd="pt__subcmd__help__subcmd__rm"
                ;;
            pt__subcmd__help,scoring)
                cmd="pt__subcmd__help__subcmd__scoring"
                ;;
            pt__subcmd__help,search)
                cmd="pt__subcmd__help__subcmd__search"
                ;;
            pt__subcmd__help,serve)
                cmd="pt__subcmd__help__subcmd__serve"
                ;;
            pt__subcmd__help,show)
                cmd="pt__subcmd__help__subcmd__show"
                ;;
            pt__subcmd__help,snooze)
                cmd="pt__subcmd__help__subcmd__snooze"
                ;;
            pt__subcmd__help,start)
                cmd="pt__subcmd__help__subcmd__start"
                ;;
            pt__subcmd__help,token)
                cmd="pt__subcmd__help__subcmd__token"
                ;;
            pt__subcmd__help,tui)
                cmd="pt__subcmd__help__subcmd__tui"
                ;;
            pt__subcmd__help,undo)
                cmd="pt__subcmd__help__subcmd__undo"
                ;;
            pt__subcmd__help,view)
                cmd="pt__subcmd__help__subcmd__view"
                ;;
            pt__subcmd__help,why)
                cmd="pt__subcmd__help__subcmd__why"
                ;;
            pt__subcmd__help__subcmd__accountability,run)
                cmd="pt__subcmd__help__subcmd__accountability__subcmd__run"
                ;;
            pt__subcmd__help__subcmd__approval,consume)
                cmd="pt__subcmd__help__subcmd__approval__subcmd__consume"
                ;;
            pt__subcmd__help__subcmd__approval,decide)
                cmd="pt__subcmd__help__subcmd__approval__subcmd__decide"
                ;;
            pt__subcmd__help__subcmd__approval,expire)
                cmd="pt__subcmd__help__subcmd__approval__subcmd__expire"
                ;;
            pt__subcmd__help__subcmd__approval,list)
                cmd="pt__subcmd__help__subcmd__approval__subcmd__list"
                ;;
            pt__subcmd__help__subcmd__approval,notify)
                cmd="pt__subcmd__help__subcmd__approval__subcmd__notify"
                ;;
            pt__subcmd__help__subcmd__approval,payload)
                cmd="pt__subcmd__help__subcmd__approval__subcmd__payload"
                ;;
            pt__subcmd__help__subcmd__approval,request)
                cmd="pt__subcmd__help__subcmd__approval__subcmd__request"
                ;;
            pt__subcmd__help__subcmd__approval,show)
                cmd="pt__subcmd__help__subcmd__approval__subcmd__show"
                ;;
            pt__subcmd__help__subcmd__approval,verify)
                cmd="pt__subcmd__help__subcmd__approval__subcmd__verify"
                ;;
            pt__subcmd__help__subcmd__approval,withdraw)
                cmd="pt__subcmd__help__subcmd__approval__subcmd__withdraw"
                ;;
            pt__subcmd__help__subcmd__goal,abandon)
                cmd="pt__subcmd__help__subcmd__goal__subcmd__abandon"
                ;;
            pt__subcmd__help__subcmd__goal,add)
                cmd="pt__subcmd__help__subcmd__goal__subcmd__add"
                ;;
            pt__subcmd__help__subcmd__goal,done)
                cmd="pt__subcmd__help__subcmd__goal__subcmd__done"
                ;;
            pt__subcmd__help__subcmd__goal,link)
                cmd="pt__subcmd__help__subcmd__goal__subcmd__link"
                ;;
            pt__subcmd__help__subcmd__goal,ls)
                cmd="pt__subcmd__help__subcmd__goal__subcmd__ls"
                ;;
            pt__subcmd__help__subcmd__goal,orphans)
                cmd="pt__subcmd__help__subcmd__goal__subcmd__orphans"
                ;;
            pt__subcmd__help__subcmd__goal,set-parent)
                cmd="pt__subcmd__help__subcmd__goal__subcmd__set__subcmd__parent"
                ;;
            pt__subcmd__help__subcmd__goal,show)
                cmd="pt__subcmd__help__subcmd__goal__subcmd__show"
                ;;
            pt__subcmd__help__subcmd__goal,unlink)
                cmd="pt__subcmd__help__subcmd__goal__subcmd__unlink"
                ;;
            pt__subcmd__help__subcmd__remote,add)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__add"
                ;;
            pt__subcmd__help__subcmd__remote,depend)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__depend"
                ;;
            pt__subcmd__help__subcmd__remote,dismiss)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__dismiss"
                ;;
            pt__subcmd__help__subcmd__remote,done)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__done"
                ;;
            pt__subcmd__help__subcmd__remote,edit)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__edit"
                ;;
            pt__subcmd__help__subcmd__remote,list)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__list"
                ;;
            pt__subcmd__help__subcmd__remote,next)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__next"
                ;;
            pt__subcmd__help__subcmd__remote,priority)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__priority"
                ;;
            pt__subcmd__help__subcmd__remote,reopen)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__reopen"
                ;;
            pt__subcmd__help__subcmd__remote,rm)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__rm"
                ;;
            pt__subcmd__help__subcmd__remote,show)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__show"
                ;;
            pt__subcmd__help__subcmd__remote,snooze)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__snooze"
                ;;
            pt__subcmd__help__subcmd__remote,start)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__start"
                ;;
            pt__subcmd__help__subcmd__remote,version)
                cmd="pt__subcmd__help__subcmd__remote__subcmd__version"
                ;;
            pt__subcmd__help__subcmd__scoring,run)
                cmd="pt__subcmd__help__subcmd__scoring__subcmd__run"
                ;;
            pt__subcmd__help__subcmd__token,create)
                cmd="pt__subcmd__help__subcmd__token__subcmd__create"
                ;;
            pt__subcmd__help__subcmd__token,list)
                cmd="pt__subcmd__help__subcmd__token__subcmd__list"
                ;;
            pt__subcmd__help__subcmd__token,revoke)
                cmd="pt__subcmd__help__subcmd__token__subcmd__revoke"
                ;;
            pt__subcmd__help__subcmd__view,list)
                cmd="pt__subcmd__help__subcmd__view__subcmd__list"
                ;;
            pt__subcmd__help__subcmd__view,rm)
                cmd="pt__subcmd__help__subcmd__view__subcmd__rm"
                ;;
            pt__subcmd__help__subcmd__view,save)
                cmd="pt__subcmd__help__subcmd__view__subcmd__save"
                ;;
            pt__subcmd__help__subcmd__view,show)
                cmd="pt__subcmd__help__subcmd__view__subcmd__show"
                ;;
            pt__subcmd__remote,add)
                cmd="pt__subcmd__remote__subcmd__add"
                ;;
            pt__subcmd__remote,depend)
                cmd="pt__subcmd__remote__subcmd__depend"
                ;;
            pt__subcmd__remote,dismiss)
                cmd="pt__subcmd__remote__subcmd__dismiss"
                ;;
            pt__subcmd__remote,done)
                cmd="pt__subcmd__remote__subcmd__done"
                ;;
            pt__subcmd__remote,edit)
                cmd="pt__subcmd__remote__subcmd__edit"
                ;;
            pt__subcmd__remote,help)
                cmd="pt__subcmd__remote__subcmd__help"
                ;;
            pt__subcmd__remote,list)
                cmd="pt__subcmd__remote__subcmd__list"
                ;;
            pt__subcmd__remote,next)
                cmd="pt__subcmd__remote__subcmd__next"
                ;;
            pt__subcmd__remote,priority)
                cmd="pt__subcmd__remote__subcmd__priority"
                ;;
            pt__subcmd__remote,reopen)
                cmd="pt__subcmd__remote__subcmd__reopen"
                ;;
            pt__subcmd__remote,rm)
                cmd="pt__subcmd__remote__subcmd__rm"
                ;;
            pt__subcmd__remote,show)
                cmd="pt__subcmd__remote__subcmd__show"
                ;;
            pt__subcmd__remote,snooze)
                cmd="pt__subcmd__remote__subcmd__snooze"
                ;;
            pt__subcmd__remote,start)
                cmd="pt__subcmd__remote__subcmd__start"
                ;;
            pt__subcmd__remote,version)
                cmd="pt__subcmd__remote__subcmd__version"
                ;;
            pt__subcmd__remote__subcmd__help,add)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__add"
                ;;
            pt__subcmd__remote__subcmd__help,depend)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__depend"
                ;;
            pt__subcmd__remote__subcmd__help,dismiss)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__dismiss"
                ;;
            pt__subcmd__remote__subcmd__help,done)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__done"
                ;;
            pt__subcmd__remote__subcmd__help,edit)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__edit"
                ;;
            pt__subcmd__remote__subcmd__help,help)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__help"
                ;;
            pt__subcmd__remote__subcmd__help,list)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__list"
                ;;
            pt__subcmd__remote__subcmd__help,next)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__next"
                ;;
            pt__subcmd__remote__subcmd__help,priority)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__priority"
                ;;
            pt__subcmd__remote__subcmd__help,reopen)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__reopen"
                ;;
            pt__subcmd__remote__subcmd__help,rm)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__rm"
                ;;
            pt__subcmd__remote__subcmd__help,show)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__show"
                ;;
            pt__subcmd__remote__subcmd__help,snooze)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__snooze"
                ;;
            pt__subcmd__remote__subcmd__help,start)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__start"
                ;;
            pt__subcmd__remote__subcmd__help,version)
                cmd="pt__subcmd__remote__subcmd__help__subcmd__version"
                ;;
            pt__subcmd__scoring,help)
                cmd="pt__subcmd__scoring__subcmd__help"
                ;;
            pt__subcmd__scoring,run)
                cmd="pt__subcmd__scoring__subcmd__run"
                ;;
            pt__subcmd__scoring__subcmd__help,help)
                cmd="pt__subcmd__scoring__subcmd__help__subcmd__help"
                ;;
            pt__subcmd__scoring__subcmd__help,run)
                cmd="pt__subcmd__scoring__subcmd__help__subcmd__run"
                ;;
            pt__subcmd__token,create)
                cmd="pt__subcmd__token__subcmd__create"
                ;;
            pt__subcmd__token,help)
                cmd="pt__subcmd__token__subcmd__help"
                ;;
            pt__subcmd__token,list)
                cmd="pt__subcmd__token__subcmd__list"
                ;;
            pt__subcmd__token,revoke)
                cmd="pt__subcmd__token__subcmd__revoke"
                ;;
            pt__subcmd__token__subcmd__help,create)
                cmd="pt__subcmd__token__subcmd__help__subcmd__create"
                ;;
            pt__subcmd__token__subcmd__help,help)
                cmd="pt__subcmd__token__subcmd__help__subcmd__help"
                ;;
            pt__subcmd__token__subcmd__help,list)
                cmd="pt__subcmd__token__subcmd__help__subcmd__list"
                ;;
            pt__subcmd__token__subcmd__help,revoke)
                cmd="pt__subcmd__token__subcmd__help__subcmd__revoke"
                ;;
            pt__subcmd__view,help)
                cmd="pt__subcmd__view__subcmd__help"
                ;;
            pt__subcmd__view,list)
                cmd="pt__subcmd__view__subcmd__list"
                ;;
            pt__subcmd__view,rm)
                cmd="pt__subcmd__view__subcmd__rm"
                ;;
            pt__subcmd__view,save)
                cmd="pt__subcmd__view__subcmd__save"
                ;;
            pt__subcmd__view,show)
                cmd="pt__subcmd__view__subcmd__show"
                ;;
            pt__subcmd__view__subcmd__help,help)
                cmd="pt__subcmd__view__subcmd__help__subcmd__help"
                ;;
            pt__subcmd__view__subcmd__help,list)
                cmd="pt__subcmd__view__subcmd__help__subcmd__list"
                ;;
            pt__subcmd__view__subcmd__help,rm)
                cmd="pt__subcmd__view__subcmd__help__subcmd__rm"
                ;;
            pt__subcmd__view__subcmd__help,save)
                cmd="pt__subcmd__view__subcmd__help__subcmd__save"
                ;;
            pt__subcmd__view__subcmd__help,show)
                cmd="pt__subcmd__view__subcmd__help__subcmd__show"
                ;;
            *)
                ;;
        esac
    done

    case "${cmd}" in
        pt)
            opts="-h -V --db --json --idempotency-key --color --no-color --help --version add list done priority edit reopen show context dismiss rm next plan view tui serve bot mcp digest export delegate branch distill accountability scoring remote promote kind start snooze reap depend review search why bulk log undo token approval goal approve reject backfill gen-manpage gen-completions help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 1 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__accountability)
            opts="-h --db --json --idempotency-key --color --no-color --help run help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__accountability__subcmd__help)
            opts="run help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__accountability__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__accountability__subcmd__help__subcmd__run)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__accountability__subcmd__run)
            opts="-h --dry-run --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__add)
            opts="-p -d -h --priority --description --deadline --reason --raw --kind --deliverable --db --json --idempotency-key --color --no-color --help <TITLE>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --priority)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -p)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --description)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -d)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --deadline)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --reason)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --kind)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --deliverable)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval)
            opts="-h --db --json --idempotency-key --color --no-color --help request list show payload withdraw verify consume expire notify decide help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__consume)
            opts="-h --payload-file --payload-json --digest --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --payload-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --payload-json)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --digest)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__decide)
            opts="-h --note --via --db --json --idempotency-key --color --no-color --help <ID> <DECISION>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --note)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --via)
                    COMPREPLY=($(compgen -W "dashboard" -- "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__expire)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help)
            opts="request list show payload withdraw verify consume expire notify decide help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__consume)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__decide)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__expire)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__list)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__notify)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__payload)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__request)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__show)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__verify)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__help__subcmd__withdraw)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__list)
            opts="-h --status --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --status)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__notify)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__payload)
            opts="-h --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__request)
            opts="-h --kind --title --note --note-file --payload-file --payload-json --digest --task --expires-in --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --kind)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --title)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --note)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --note-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --payload-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --payload-json)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --digest)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --task)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --expires-in)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__show)
            opts="-h --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__verify)
            opts="-h --payload-file --payload-json --digest --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --payload-file)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --payload-json)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --digest)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approval__subcmd__withdraw)
            opts="-h --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__approve)
            opts="-h --note --via --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --note)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --via)
                    COMPREPLY=($(compgen -W "dashboard" -- "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__backfill)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__bot)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__branch)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__bulk)
            opts="-h --set-priority --done --dismiss --dry-run --db --json --idempotency-key --color --no-color --help <FILTER>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --set-priority)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__context)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__delegate)
            opts="-h --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__depend)
            opts="-h --on --clear --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --on)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__digest)
            opts="-h --days --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --days)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__dismiss)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__distill)
            opts="-h --batch --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --batch)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__done)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERIES>..."
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__edit)
            opts="-h --deadline --clear-deadline --title --desc --label --unlabel --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --deadline)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --title)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --desc)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --label)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --unlabel)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__export)
            opts="-h --out --git --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --out)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__gen__subcmd__completions)
            opts="-h --db --json --idempotency-key --color --no-color --help bash zsh fish"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__gen__subcmd__manpage)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal)
            opts="-h --db --json --idempotency-key --color --no-color --help add ls show link unlink set-parent done abandon orphans help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__abandon)
            opts="-h --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__add)
            opts="-h --why --parent --db --json --idempotency-key --color --no-color --help <TITLE>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --why)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --parent)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__done)
            opts="-h --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help)
            opts="add ls show link unlink set-parent done abandon orphans help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help__subcmd__abandon)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help__subcmd__add)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help__subcmd__done)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help__subcmd__link)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help__subcmd__ls)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help__subcmd__orphans)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help__subcmd__set__subcmd__parent)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help__subcmd__show)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__help__subcmd__unlink)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__link)
            opts="-h --db --json --idempotency-key --color --no-color --help <TASK> <GOAL>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__ls)
            opts="-h --all --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__orphans)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__set__subcmd__parent)
            opts="-h --db --json --idempotency-key --color --no-color --help <ID> <PARENT>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__show)
            opts="-h --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__goal__subcmd__unlink)
            opts="-h --db --json --idempotency-key --color --no-color --help <TASK>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help)
            opts="add list done priority edit reopen show context dismiss rm next plan view tui serve bot mcp digest export delegate branch distill accountability scoring remote promote kind start snooze reap depend review search why bulk log undo token approval goal approve reject backfill gen-manpage gen-completions help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__accountability)
            opts="run"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__accountability__subcmd__run)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__add)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval)
            opts="request list show payload withdraw verify consume expire notify decide"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval__subcmd__consume)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval__subcmd__decide)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval__subcmd__expire)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval__subcmd__list)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval__subcmd__notify)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval__subcmd__payload)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval__subcmd__request)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval__subcmd__show)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval__subcmd__verify)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approval__subcmd__withdraw)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__approve)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__backfill)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__bot)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__branch)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__bulk)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__context)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__delegate)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__depend)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__digest)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__dismiss)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__distill)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__done)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__edit)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__export)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__gen__subcmd__completions)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__gen__subcmd__manpage)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__goal)
            opts="add ls show link unlink set-parent done abandon orphans"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__goal__subcmd__abandon)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__goal__subcmd__add)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__goal__subcmd__done)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__goal__subcmd__link)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__goal__subcmd__ls)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__goal__subcmd__orphans)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__goal__subcmd__set__subcmd__parent)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__goal__subcmd__show)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__goal__subcmd__unlink)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__kind)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__list)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__log)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__mcp)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__next)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__plan)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__priority)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__promote)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__reap)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__reject)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote)
            opts="add list done priority edit reopen show next dismiss start snooze depend rm version"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__add)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__depend)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__dismiss)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__done)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__edit)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__list)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__next)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__priority)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__reopen)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__rm)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__show)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__snooze)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__start)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__remote__subcmd__version)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__reopen)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__review)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__rm)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__scoring)
            opts="run"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__scoring__subcmd__run)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__search)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__serve)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__show)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__snooze)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__start)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__token)
            opts="create list revoke"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__token__subcmd__create)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__token__subcmd__list)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__token__subcmd__revoke)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__tui)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__undo)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__view)
            opts="save list show rm"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__view__subcmd__list)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__view__subcmd__rm)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__view__subcmd__save)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__view__subcmd__show)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__help__subcmd__why)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__kind)
            opts="-h --deliverable --db --json --idempotency-key --color --no-color --help <QUERY> <KIND>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --deliverable)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__list)
            opts="-s -p -n -v -h --status --priority --limit --verbose --sort --db --json --idempotency-key --color --no-color --help [FILTER]"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --status)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -s)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --priority)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -p)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -n)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --sort)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__log)
            opts="-n -h --limit --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -n)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__mcp)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__next)
            opts="-n -h --limit --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -n)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__plan)
            opts="-n -h --account --days --work --tz --calendar --slot-default --limit --write --gcal --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --account)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --days)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --work)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --tz)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --calendar)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --slot-default)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -n)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --gcal)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__priority)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERY> <LEVEL>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__promote)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__reap)
            opts="-h --dry-run --json --db --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__reject)
            opts="-h --note --via --db --json --idempotency-key --color --no-color --help <ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --note)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --via)
                    COMPREPLY=($(compgen -W "dashboard" -- "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote)
            opts="-h --db --json --idempotency-key --color --no-color --help add list done priority edit reopen show next dismiss start snooze depend rm version help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__add)
            opts="-h --url --db --json --idempotency-key --color --no-color --help <TEXT>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__depend)
            opts="-h --on --clear --url --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --on)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__dismiss)
            opts="-h --url --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__done)
            opts="-h --url --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__edit)
            opts="-h --deadline --clear-deadline --title --desc --url --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --deadline)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --title)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --desc)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help)
            opts="add list done priority edit reopen show next dismiss start snooze depend rm version help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__add)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__depend)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__dismiss)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__done)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__edit)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__list)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__next)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__priority)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__reopen)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__rm)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__show)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__snooze)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__start)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__help__subcmd__version)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__list)
            opts="-s -f -p -n -h --status --filter --priority --limit --url --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --status)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -s)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --filter)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -f)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --priority)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -p)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -n)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__next)
            opts="-n -h --limit --url --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -n)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__priority)
            opts="-h --url --db --json --idempotency-key --color --no-color --help <QUERY> <LEVEL>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__reopen)
            opts="-h --url --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__rm)
            opts="-h --url --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__show)
            opts="-h --url --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__snooze)
            opts="-h --url --db --json --idempotency-key --color --no-color --help <QUERY> [UNTIL]..."
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__start)
            opts="-h --url --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__remote__subcmd__version)
            opts="-h --url --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --url)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__reopen)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__review)
            opts="-h --stale-days --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --stale-days)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__rm)
            opts="-y -h --yes --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__scoring)
            opts="-h --db --json --idempotency-key --color --no-color --help run help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__scoring__subcmd__help)
            opts="run help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__scoring__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__scoring__subcmd__help__subcmd__run)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__scoring__subcmd__run)
            opts="-h --dry-run --v1 --diff --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__search)
            opts="-n -h --limit --db --json --idempotency-key --color --no-color --help [QUERY]..."
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -n)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__serve)
            opts="-h --bind --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --bind)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__show)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__snooze)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERY> [UNTIL]..."
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__start)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__token)
            opts="-h --db --json --idempotency-key --color --no-color --help create list revoke help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__token__subcmd__create)
            opts="-h --scope --db --json --idempotency-key --color --no-color --help <CLIENT_ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --scope)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__token__subcmd__help)
            opts="create list revoke help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__token__subcmd__help__subcmd__create)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__token__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__token__subcmd__help__subcmd__list)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__token__subcmd__help__subcmd__revoke)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__token__subcmd__list)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__token__subcmd__revoke)
            opts="-h --db --json --idempotency-key --color --no-color --help <CLIENT_ID>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__tui)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__undo)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view)
            opts="-h --db --json --idempotency-key --color --no-color --help save list show rm help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view__subcmd__help)
            opts="save list show rm help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view__subcmd__help__subcmd__list)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view__subcmd__help__subcmd__rm)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view__subcmd__help__subcmd__save)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view__subcmd__help__subcmd__show)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 4 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view__subcmd__list)
            opts="-h --db --json --idempotency-key --color --no-color --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view__subcmd__rm)
            opts="-h --db --json --idempotency-key --color --no-color --help <NAME>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view__subcmd__save)
            opts="-h --db --json --idempotency-key --color --no-color --help <NAME> <FILTER>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__view__subcmd__show)
            opts="-n -h --limit --db --json --idempotency-key --color --no-color --help <NAME>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --limit)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                -n)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        pt__subcmd__why)
            opts="-h --db --json --idempotency-key --color --no-color --help <QUERY>"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --db)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --idempotency-key)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --color)
                    COMPREPLY=($(compgen -W "auto always never" -- "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
    esac
}

if [[ "${BASH_VERSINFO[0]}" -eq 4 && "${BASH_VERSINFO[1]}" -ge 4 || "${BASH_VERSINFO[0]}" -gt 4 ]]; then
    complete -F _pt -o nosort -o bashdefault -o default pt
else
    complete -F _pt -o bashdefault -o default pt
fi
