# ~/.zshrc
HISTFILE=~/.zsh_history
HISTSIZE=10000
SAVEHIST=10000
setopt HIST_IGNORE_DUPS SHARE_HISTORY

autoload -Uz compinit && compinit

export EDITOR=nvim
alias ll='ls -lah'
alias gs='git status -sb'

PROMPT='%n@%m:%~ %# '
