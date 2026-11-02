#!/usr/bin/env perl
use strict;
use warnings;

my $found = 0;

sub rust_comment {
    my ($line) = @_;
    my $code = $line;
    $code =~ s/r(#*)".*?"\1//g;
    $code =~ s/"(?:\\.|[^"\\])*"//g;
    $code =~ s/'(?:\\.|[^'\\])'//g;
    return $code =~ m{//|/\*};
}

sub toml_comment {
    my ($line, $state) = @_;
    my $length = length $line;
    my $index = 0;

    while ($index < $length) {
        my $char = substr $line, $index, 1;
        my $triple = substr $line, $index, 3;

        if ($$state eq 'basic') {
            if ($char eq '\\') {
                $index++;
            } elsif ($char eq '"') {
                $$state = 'code';
            }
        } elsif ($$state eq 'literal') {
            $$state = 'code' if $char eq "'";
        } elsif ($$state eq 'multi_basic') {
            if ($char eq '\\') {
                $index++;
            } elsif ($triple eq '"""') {
                $$state = 'code';
                $index += 2;
            }
        } elsif ($$state eq 'multi_literal') {
            if ($triple eq "'''") {
                $$state = 'code';
                $index += 2;
            }
        } elsif ($triple eq '"""') {
            $$state = 'multi_basic';
            $index += 2;
        } elsif ($triple eq "'''") {
            $$state = 'multi_literal';
            $index += 2;
        } elsif ($char eq '"') {
            $$state = 'basic';
        } elsif ($char eq "'") {
            $$state = 'literal';
        } elsif ($char eq '#') {
            return 1;
        }

        $index++;
    }

    $$state = 'code' if $$state eq 'basic' || $$state eq 'literal';
    return 0;
}

for my $path (@ARGV) {
    open my $handle, '<', $path or die "cannot read $path: $!\n";
    my $is_toml = $path =~ /\.toml\z/;
    my $state = 'code';
    while (my $line = <$handle>) {
        my $comment = $is_toml ? toml_comment($line, \$state) : rust_comment($line);
        next unless $comment;
        print "$path:$.: $line";
        $found = 1;
    }
    close $handle;
}

exit $found;
