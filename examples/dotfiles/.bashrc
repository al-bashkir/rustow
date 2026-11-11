# ~/.bashrc
case $- in
    *i*) ;;
    *) return ;;
esac

HISTSIZE=10000
HISTFILESIZE=20000
HISTCONTROL=ignoreboth
shopt -s histappend checkwinsize

export EDITOR=nvim
alias ll='ls -lah'
alias gs='git status -sb'

PS1='\u@\h:\w\$ '
